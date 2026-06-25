//! Receive-side clock-drift compensation for the native voice transport.
//!
//! The sender's capture sample-clock and our playback sample-clock are separate
//! crystals that never run at exactly the same rate (cheap USB audio and Windows
//! shared-mode mixing are commonly ~1% off). Left alone, that skew makes the
//! receive buffer slowly fill or drain; the dejitter buffer then either drops a
//! frame (sender fast) or starves (sender slow) every second or so, and each such
//! discontinuity is an audible click — the "beep" on a clean, zero-loss link.
//!
//! The fix here is a varispeed playout that gently stretches or compresses the
//! decoded audio by the drift ratio so the buffer holds steady. A [`DriftServo`]
//! watches the buffered depth and outputs a playback speed near 1.0; a
//! [`Varispeed`] linear resampler applies it to the decoded PCM. The correction
//! is a continuous ~1% pitch nudge (~0.17 semitone, imperceptible for voice),
//! so no frame is ever dropped during normal operation.

/// Playback-speed lower bound. Below 1.0 stretches audio (slows playout) to let a
/// draining buffer refill. ±3% comfortably covers the ~1% skew seen in practice.
const MIN_SPEED: f32 = 0.97;

/// Playback-speed upper bound. Above 1.0 compresses audio (speeds playout) to
/// drain a filling buffer before it overflows.
const MAX_SPEED: f32 = 1.03;

/// Depth smoothing (one-pole). The instantaneous buffered depth is noisy frame to
/// frame; drift is a slow DC offset, so the servo controls on this average. Kept
/// well faster than the control pole so its lag can't drive the loop oscillatory.
const DEPTH_EMA: f32 = 0.05;

/// Proportional gain: speed deviation per frame of depth error. Small enough that
/// the integrator-plus-proportional loop stays first-order (overdamped, no ring).
const GAIN: f32 = 0.008;

/// Max speed change per observation. Slew-limiting keeps the pitch nudge gradual
/// (no audible step) and adds damping margin.
const SLEW: f32 = 0.0006;

/// Watches the receive buffer depth and outputs a playback speed (~1.0) that
/// nudges the depth back toward `target`. Proportional control of the buffer (a
/// pure integrator) gives a stable first-order response: depth settles at a small
/// steady offset and speed converges on the true drift ratio.
pub struct DriftServo {
    /// Desired buffered depth, in frames.
    target: f32,
    /// Smoothed observed depth.
    avg_depth: f32,
    /// False until the first observation seeds `avg_depth` (so it doesn't ramp up
    /// from zero on the first call).
    seeded: bool,
    /// Current playback speed, slewed toward the proportional target each tick.
    speed: f32,
}

impl DriftServo {
    pub fn new(target_frames: f32) -> DriftServo {
        DriftServo { target: target_frames, avg_depth: 0.0, seeded: false, speed: 1.0 }
    }

    /// Update the desired buffered depth (frames). Live.
    pub fn set_target(&mut self, target_frames: f32) {
        self.target = target_frames;
    }

    /// Most recently computed playback speed.
    pub fn speed(&self) -> f32 {
        self.speed
    }

    /// Feed the current buffered depth (frames); returns the playback speed to
    /// apply until the next observation.
    pub fn observe(&mut self, depth_frames: f32) -> f32 {
        if self.seeded {
            self.avg_depth += DEPTH_EMA * (depth_frames - self.avg_depth);
        } else {
            self.avg_depth = depth_frames;
            self.seeded = true;
        }

        let desired = (1.0 + GAIN * (self.avg_depth - self.target)).clamp(MIN_SPEED, MAX_SPEED);

        let delta = (desired - self.speed).clamp(-SLEW, SLEW);
        self.speed += delta;

        self.speed
    }
}

/// Linear-interpolating varispeed resampler for a continuous mono stream.
///
/// `process` reads `input` at a fractional step of `speed` per output sample
/// (`speed > 1` yields fewer samples, `< 1` more) and appends the result to
/// `out`. The fractional read position and the previous chunk's final sample are
/// carried across calls, so interpolation spans the chunk boundary and the output
/// stays continuous (no seam click).
pub struct Varispeed {
    /// Fractional read position relative to the start of the next input chunk; can
    /// be negative, meaning "interpolate from `last` into the new chunk".
    pos: f32,
    /// Final sample of the previous chunk, the left point for a boundary-spanning
    /// interpolation.
    last: f32,
}

impl Default for Varispeed {
    fn default() -> Varispeed {
        Varispeed::new()
    }
}

impl Varispeed {
    pub fn new() -> Varispeed {
        Varispeed { pos: 0.0, last: 0.0 }
    }

    /// Resample `input` at `speed` (precondition: `speed > 0`), appending to `out`.
    pub fn process(&mut self, input: &[f32], speed: f32, out: &mut Vec<f32>) {
        if input.is_empty() {
            return;
        }

        let n = input.len();
        let last_index = (n - 1) as f32;
        let mut pos = self.pos;

        while pos <= last_index {
            let i = pos.floor() as isize;
            let frac = pos - i as f32;

            let a = if i < 0 { self.last } else { input[i as usize] };
            let bi = i + 1;
            let b = if bi < 0 {
                self.last
            } else if (bi as usize) < n {
                input[bi as usize]
            } else {
                a // only reached at frac == 0, so `b` is unused weight-wise
            };

            out.push(a + frac * (b - a));
            pos += speed;
        }

        self.last = input[n - 1];
        self.pos = pos - n as f32;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn servo_is_neutral_when_depth_sits_at_target() {
        let mut s = DriftServo::new(2.0);
        let mut speed = 1.0;
        for _ in 0..1000 {
            speed = s.observe(2.0);
        }
        assert!((speed - 1.0).abs() < 1.0e-3, "no correction at target: {speed}");
    }

    #[test]
    fn servo_speeds_up_when_buffer_is_too_full() {
        let mut s = DriftServo::new(2.0);
        let mut speed = 1.0;
        for _ in 0..2000 {
            speed = s.observe(6.0);
        }
        assert!(speed > 1.0, "a too-full buffer must drain faster: {speed}");
    }

    #[test]
    fn servo_slows_down_when_buffer_is_too_low() {
        let mut s = DriftServo::new(4.0);
        let mut speed = 1.0;
        for _ in 0..2000 {
            speed = s.observe(0.0);
        }
        assert!(speed < 1.0, "a draining buffer must play slower to refill: {speed}");
    }

    #[test]
    fn servo_speed_stays_within_bounds() {
        let mut s = DriftServo::new(2.0);
        let mut speed = 1.0;
        for _ in 0..5000 {
            speed = s.observe(100.0);
        }
        assert!(speed <= MAX_SPEED + 1.0e-6, "clamped to the ceiling: {speed}");

        let mut s = DriftServo::new(100.0);
        let mut speed = 1.0;
        for _ in 0..5000 {
            speed = s.observe(0.0);
        }
        assert!(speed >= MIN_SPEED - 1.0e-6, "clamped to the floor: {speed}");
    }

    #[test]
    fn servo_slews_rather_than_jumping() {
        let mut s = DriftServo::new(2.0);
        // A huge error on the very first tick still moves speed by at most SLEW.
        let speed = s.observe(100.0);
        assert!((speed - 1.0).abs() <= SLEW + 1.0e-9, "first step is slew-limited: {speed}");
    }

    #[test]
    fn servo_converges_on_the_drift_ratio_without_oscillating() {
        // Model the buffer as a pure integrator: the sender supplies `sender`
        // frames per tick, we drain `speed` frames per tick, so depth integrates
        // their difference. A 1.2%-fast sender must be tracked without the depth
        // ringing or running away.
        let target = 3.0;
        let sender = 1.012;

        let mut s = DriftServo::new(target);
        let mut depth = target;
        let mut speed;
        let mut max_dev: f32 = 0.0;

        for tick in 0..8000 {
            speed = s.observe(depth);
            depth += sender - speed;
            if depth < 0.0 {
                depth = 0.0; // a real buffer can't go below empty
            }
            if tick > 500 {
                max_dev = max_dev.max((depth - target).abs());
            }
        }

        assert!(
            (s.speed() - sender).abs() < 0.004,
            "speed converges on the true drift ratio: {} vs {sender}",
            s.speed()
        );
        assert!(max_dev < 6.0, "depth stays bounded near target, no oscillation: {max_dev}");
    }

    fn approx_eq(a: &[f32], b: &[f32]) -> bool {
        a.len() == b.len() && a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1.0e-5)
    }

    #[test]
    fn varispeed_unity_is_exact_passthrough() {
        let mut v = Varispeed::new();
        let mut out = Vec::new();
        v.process(&[1.0, 2.0, 3.0, 4.0], 1.0, &mut out);
        assert!(approx_eq(&out, &[1.0, 2.0, 3.0, 4.0]), "unity speed passes samples through: {out:?}");
    }

    #[test]
    fn varispeed_unity_is_continuous_across_calls() {
        let mut v = Varispeed::new();
        let mut out = Vec::new();
        v.process(&[1.0, 2.0, 3.0, 4.0], 1.0, &mut out);
        v.process(&[5.0, 6.0, 7.0, 8.0], 1.0, &mut out);
        assert!(approx_eq(&out, &[1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0]), "{out:?}");
    }

    #[test]
    fn varispeed_faster_than_unity_yields_fewer_samples() {
        let mut v = Varispeed::new();
        let input: Vec<f32> = (0..100).map(|i| i as f32).collect();
        let mut out = Vec::new();
        v.process(&input, 2.0, &mut out);
        assert!((out.len() as i32 - 50).abs() <= 1, "≈half as many samples: {}", out.len());
    }

    #[test]
    fn varispeed_slower_than_unity_yields_more_samples() {
        let mut v = Varispeed::new();
        let input: Vec<f32> = (0..100).map(|i| i as f32).collect();
        let mut out = Vec::new();
        v.process(&input, 0.5, &mut out);
        assert!((out.len() as i32 - 200).abs() <= 2, "≈twice as many samples: {}", out.len());
    }

    #[test]
    fn varispeed_has_no_discontinuity_at_the_chunk_seam() {
        // A linear ramp resampled in two halves must stay a single linear ramp:
        // every step equal, including the one straddling the call boundary.
        let mut v = Varispeed::new();
        let mut out = Vec::new();
        v.process(&[0.0, 1.0, 2.0, 3.0], 0.5, &mut out);
        v.process(&[4.0, 5.0, 6.0, 7.0], 0.5, &mut out);

        for w in out.windows(2) {
            let step = w[1] - w[0];
            assert!((step - 0.5).abs() < 1.0e-4, "uniform step across the seam: {step}");
        }
    }

    #[test]
    fn varispeed_preserves_a_constant_signal() {
        let mut v = Varispeed::new();
        let mut out = Vec::new();
        v.process(&[0.7; 50], 1.013, &mut out);
        for s in &out {
            assert!((s - 0.7).abs() < 1.0e-4, "DC is preserved under resampling: {s}");
        }
    }
}
