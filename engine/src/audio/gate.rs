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
    ptt_held: bool,
    last_rms_db: f32,
    last_vad: bool,
}

impl Gate {
    pub fn new(mode: ActivationMode) -> Gate {
        Gate { mode, muted: false, ptt_held: false, last_rms_db: -120.0, last_vad: false }
    }

    pub fn set_mode(&mut self, mode: ActivationMode) {
        self.mode = mode;
    }

    pub fn set_muted(&mut self, muted: bool) {
        self.muted = muted;
    }

    pub fn set_ptt_held(&mut self, held: bool) {
        self.ptt_held = held;
    }

    pub fn update_level(&mut self, rms_db: f32, vad: bool) {
        self.last_rms_db = rms_db;
        self.last_vad = vad;
    }

    /// True = transmit. Precedence: mute > ptt > voice-activity > always-on.
    pub fn open(&self) -> bool {
        if self.muted {
            return false;
        }

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
