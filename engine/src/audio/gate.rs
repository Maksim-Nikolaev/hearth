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
    hold_remaining: u32,
}

/// Stay open this many 5 ms frames after the level last cleared the threshold
/// (~150 ms), so gaps between words don't close the gate and the boundary doesn't
/// chatter — without holding the gate open *below* the threshold.
const HOLD_FRAMES: u32 = 30;

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

        // Hold-gate: open the moment the level clears the threshold, then keep it
        // open for HOLD_FRAMES after it last cleared. The hold both bridges word
        // gaps and prevents boundary chatter (a single poke above re-arms it), but
        // it never holds the gate open while the level sits below the threshold.
        if rms_db >= self.sensitivity_db || vad {
            self.voice_active = true;
            self.hold_remaining = HOLD_FRAMES;
        } else if self.hold_remaining > 0 {
            self.hold_remaining -= 1;
        } else {
            self.voice_active = false;
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
        assert!(g.open(), "above threshold = open");
        g.update_level(-60.0, true);
        assert!(g.open(), "vad voice flag = open even if quiet");
    }
}
