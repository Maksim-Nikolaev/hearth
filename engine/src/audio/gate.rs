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
}

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
        self.last_rms_db >= self.sensitivity_db
    }

    fn mode_open(&self) -> bool {
        match self.mode {
            ActivationMode::PushToTalk => self.ptt_held,
            ActivationMode::Voice { threshold } => self.last_rms_db >= threshold || self.last_vad,
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
