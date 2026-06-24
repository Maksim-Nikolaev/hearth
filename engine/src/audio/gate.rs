#[derive(Debug, Clone, Copy)]
pub enum ActivationMode {
    /// Open when input RMS exceeds `threshold` dBFS or the VAD reports voice.
    Voice { threshold: f32 },
    PushToTalk,
    AlwaysOn,
}

pub struct Gate {
    mode: ActivationMode,
    muted: bool,
    /// Temporary force-closed, independent of `muted` (e.g. while the Settings
    /// window is open). Auto-restores the user's real state when cleared.
    suspended: bool,
    ptt_held: bool,
    last_rms_db: f32,
    last_vad: bool,
    /// Voice-activity sensitivity (dBFS). Tracks the level-bar handle in all
    /// modes so the mic-test monitor can gate by it regardless of activation mode.
    sensitivity_db: f32,
    /// Debounced voice-activity state (hysteresis + hold), so the gate doesn't
    /// chatter when the level hovers near the threshold.
    voice_active: bool,
    /// Frames left in the hold window; counts down once the level is below the
    /// close threshold, and is refreshed to `hold_frames` while voice is present.
    hold_remaining: u32,
    /// Configured hold length in frames (see `DEFAULT_HOLD_FRAMES`).
    hold_frames: u32,
}

/// Default post-speech hold: after the level falls below the *close* threshold,
/// stay fully open this many frames before the release fade is allowed to begin.
/// Bridges the short gaps between words/syllables so a normal speech pause
/// doesn't trip the gate. ~120 ms on the native path (5 ms frames). Settable
/// per-session via [`Gate::set_hold_frames`]; 100–300 ms is the usual range.
const DEFAULT_HOLD_FRAMES: u32 = 24;

/// Hysteresis dead band (dB): once open, the level must fall this far below the
/// open threshold before the gate begins to close — open at `sensitivity_db`,
/// close at `sensitivity_db - HYSTERESIS_DB`. The dead band stops the open/close
/// decision dithering (chattering) when the level rides on the threshold. Kept
/// modest so the close threshold stays above a typical room-noise floor and the
/// gate still shuts in a quiet room.
const HYSTERESIS_DB: f32 = 6.0;

impl Gate {
    pub fn new(mode: ActivationMode) -> Gate {
        let sensitivity_db = match mode {
            ActivationMode::Voice { threshold } => threshold,
            _ => -45.0,
        };
        Gate {
            mode,
            muted: false,
            suspended: false,
            ptt_held: false,
            last_rms_db: -120.0,
            last_vad: false,
            sensitivity_db,
            voice_active: false,
            hold_remaining: 0,
            hold_frames: DEFAULT_HOLD_FRAMES,
        }
    }

    pub fn set_mode(&mut self, mode: ActivationMode) {
        // Keep the sensitivity in sync when switching into Voice mode.
        if let ActivationMode::Voice { threshold } = mode {
            self.sensitivity_db = threshold;
        }
        self.mode = mode;
    }

    /// Set the voice-activity sensitivity (the level-bar handle), independent of
    /// the active mode — drives the mic-test monitor gating.
    pub fn set_sensitivity(&mut self, db: f32) {
        self.sensitivity_db = db;
    }

    /// Set the post-speech hold length in frames (0 = no hold). Live; the smooth
    /// release fade in `ramp_gain` still applies once the hold elapses.
    pub fn set_hold_frames(&mut self, frames: u32) {
        self.hold_frames = frames;
    }

    pub fn set_muted(&mut self, muted: bool) {
        self.muted = muted;
    }

    pub fn set_suspended(&mut self, suspended: bool) {
        self.suspended = suspended;
    }

    pub fn set_ptt_held(&mut self, held: bool) {
        self.ptt_held = held;
    }

    pub fn update_level(&mut self, rms_db: f32, vad: bool) {
        self.last_rms_db = rms_db;
        self.last_vad = vad;

        // Hysteretic hold-gate — this is only the open/close *decision*; the
        // smooth gain envelope (attack / exponential release / floor) is
        // `ramp_gain`. Open at `sensitivity_db`, close at the lower `close_thresh`;
        // the dead band between them is what stops edge chatter. Once open, hold
        // full-open for `hold_frames` after the level drops below close (bridging
        // word gaps), then let the release fade take over.
        let open_thresh = self.sensitivity_db;
        let close_thresh = self.sensitivity_db - HYSTERESIS_DB;

        if self.voice_active {
            if rms_db >= close_thresh || vad {
                self.hold_remaining = self.hold_frames;
            } else if self.hold_remaining > 0 {
                self.hold_remaining -= 1;
            } else {
                self.voice_active = false;
            }
        } else if rms_db >= open_thresh || vad {
            self.voice_active = true;
            self.hold_remaining = self.hold_frames;
        }
    }

    /// True = transmit. Precedence: suspend > mute > ptt > voice-activity > always-on.
    pub fn open(&self) -> bool {
        if self.suspended || self.muted {
            return false;
        }
        self.mode_open()
    }

    /// Mic-test monitor gating: hear yourself only when the level clears the
    /// sensitivity handle. Threshold-based in every mode (so you can tune
    /// sensitivity by ear), and ignores mute / the Settings-open suspend.
    pub fn monitor_open(&self) -> bool {
        self.voice_active
    }

    fn mode_open(&self) -> bool {
        match self.mode {
            ActivationMode::PushToTalk => self.ptt_held,
            ActivationMode::Voice { .. } => self.voice_active,
            ActivationMode::AlwaysOn => true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mute_overrides_everything() {
        let mut g = Gate::new(ActivationMode::AlwaysOn);
        assert!(g.open());
        g.set_muted(true);
        assert!(!g.open());
    }

    #[test]
    fn ptt_gates_on_key() {
        let mut g = Gate::new(ActivationMode::PushToTalk);
        assert!(!g.open());
        g.set_ptt_held(true);
        assert!(g.open());
        g.set_ptt_held(false);
        assert!(!g.open());
    }

    #[test]
    fn voice_activity_uses_threshold_or_vad() {
        let mut g = Gate::new(ActivationMode::Voice { threshold: -40.0 });
        g.update_level(-60.0, false);
        assert!(!g.open(), "below threshold + no vad = closed");
        g.update_level(-30.0, false);
        assert!(g.open(), "above the open threshold = open");
        g.update_level(-30.0, true);
        assert!(g.open(), "vad voice flag keeps it open");
    }

    #[test]
    fn vad_opens_when_quiet() {
        let mut g = Gate::new(ActivationMode::Voice { threshold: -40.0 });
        g.update_level(-55.0, true);
        assert!(g.open(), "detected voice opens even below the threshold");
    }

    #[test]
    fn hysteresis_dead_band_prevents_chatter() {
        let mut g = Gate::new(ActivationMode::Voice { threshold: -40.0 });
        g.update_level(-35.0, false); // clears the open threshold (-40)
        assert!(g.open());
        // Level then rides in the dead band: below open (-40) but above close
        // (-46). It must stay open — this is what kills edge chatter (the buzz).
        for _ in 0..(DEFAULT_HOLD_FRAMES * 4) {
            g.update_level(-43.0, false);
        }
        assert!(g.open(), "rides the hysteresis band without dithering shut");
    }

    #[test]
    fn closes_after_hold_below_close_threshold() {
        let mut g = Gate::new(ActivationMode::Voice { threshold: -40.0 });
        g.set_hold_frames(8);
        g.update_level(-30.0, false);
        assert!(g.open());
        // Well below the close threshold (-46): bridges the hold, then shuts.
        for _ in 0..8 {
            g.update_level(-70.0, false);
            assert!(g.open(), "held open across the hold window");
        }
        g.update_level(-70.0, false);
        assert!(!g.open(), "closes once the hold elapses");
    }

    #[test]
    fn hold_frames_zero_closes_at_once() {
        let mut g = Gate::new(ActivationMode::Voice { threshold: -40.0 });
        g.set_hold_frames(0);
        g.update_level(-30.0, false);
        assert!(g.open());
        g.update_level(-70.0, false);
        assert!(!g.open(), "with the hold off, dropping below close shuts immediately");
    }
}
