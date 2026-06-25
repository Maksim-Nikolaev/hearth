//! Native low-latency audio I/O — the platform device backends plus the shared,
//! platform-independent pieces (mixer constants, the soft-clip limiter, the RMS
//! meter, and the mic-test monitor).
//!
//! The concrete `NativeCapture`/`NativePlayback` are WASAPI on Windows
//! (`native_wasapi`) and PipeWire on Linux (`native_pw`). Both expose the same
//! API — `start` / `push` / `far_end` / `remove_source`, delivering mono f32 @
//! 48 kHz — so `native_voice.rs` (DSP → Opus → UDP) is platform-independent.

/// Working sample rate the rest of the engine (Opus, DSP) expects.
pub const SAMPLE_RATE: u32 = 48000;

/// Max per-source playback backlog (~20 ms). Caps the mixer-lane latency; the
/// shared engine clock means no drift to buffer against, so keep it shallow.
pub(crate) const MAX_LANE_SAMPLES: usize = (SAMPLE_RATE as usize) * 20 / 1000;

/// Cap the far-end ring at ~200 ms so it can't grow unbounded when no AEC is
/// consuming it (drop oldest).
// Consumed by each backend's playback render loop (WASAPI directly, PipeWire in
// Task 5); `allow` keeps the Linux-only interim build clean until then.
#[cfg_attr(target_os = "linux", allow(dead_code))]
pub(crate) const FAR_END_CAP: usize = SAMPLE_RATE as usize / 5;

// ── Platform device backend ───────────────────────────────────────────────────

#[cfg(windows)]
mod native_wasapi;
#[cfg(windows)]
pub(crate) use native_wasapi::{NativeCapture, NativePlayback};

#[cfg(target_os = "linux")]
mod native_pw;
#[cfg(target_os = "linux")]
pub(crate) use native_pw::{NativeCapture, NativePlayback};

/// Gentle limiter for the summed mix: identity up to ±0.95, then a smooth tanh
/// knee asymptoting to ±1.0. Avoids the harsh square-wave distortion of a
/// brick-wall clamp (which also drives the acoustic echo loop harder).
pub(crate) fn soft_clip(x: f32) -> f32 {
    const T: f32 = 0.95;
    let a = x.abs();
    if a <= T {
        x
    } else {
        x.signum() * (T + (1.0 - T) * ((a - T) / (1.0 - T)).tanh())
    }
}

#[cfg(test)]
mod tests {
    use audiopus::{coder::Decoder, coder::Encoder, Application, Channels, SampleRate};

    // De-risk: confirm the bundled libopus links and round-trips at runtime
    // (10 ms mono @ 48 kHz, low-delay) — the codec the native voice path uses.
    #[test]
    fn opus_lowdelay_roundtrip() {
        let enc = Encoder::new(SampleRate::Hz48000, Channels::Mono, Application::LowDelay).unwrap();
        let mut dec = Decoder::new(SampleRate::Hz48000, Channels::Mono).unwrap();

        let pcm: Vec<f32> = (0..480).map(|i| (i as f32 * 0.05).sin() * 0.25).collect();
        let mut packet = vec![0u8; 4000];
        let n = enc.encode_float(&pcm, &mut packet).unwrap();
        assert!(n > 0 && n < 4000);

        let mut out = vec![0.0f32; 480];
        let frames = dec.decode_float(Some(&packet[..n]), &mut out, false).unwrap();
        assert_eq!(frames, 480);
    }
}
