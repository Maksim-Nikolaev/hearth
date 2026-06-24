use crate::audio::dsp::{DspConfig, NsLevel};

/// User-facing voice processing profile. `Custom` is the user's hand-tuned
/// config; the presets are read-only views; `Auto` resolves from the output
/// device's form factor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VoiceProfile {
    Custom,
    Headset,
    Speaker,
    Auto,
}

/// Acoustic class of the active output device, used to resolve `Auto`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputKind {
    Headphones,
    Speakers,
    Unknown,
}

/// Headset: no AEC (mic can't hear the headphones), NS/AGC/HPF on. Lowest latency.
pub fn headset_preset() -> DspConfig {
    DspConfig {
        echo_cancel: false,
        noise_suppression: NsLevel::Moderate,
        agc: true,
        vad: true,
        high_pass: true,
    }
}

/// Speaker: full processing including AEC for the open-air echo path.
pub fn speaker_preset() -> DspConfig {
    DspConfig {
        echo_cancel: true,
        noise_suppression: NsLevel::Moderate,
        agc: true,
        vad: true,
        high_pass: true,
    }
}

/// Resolve a classification to a preset. `Unknown` is the safe low-latency default.
pub fn preset_for(kind: OutputKind) -> DspConfig {
    match kind {
        OutputKind::Speakers => speaker_preset(),
        OutputKind::Headphones | OutputKind::Unknown => headset_preset(),
    }
}

/// The effective DSP config the engine should run for the given profile.
pub fn effective(profile: VoiceProfile, custom: &DspConfig, output: OutputKind) -> DspConfig {
    match profile {
        VoiceProfile::Custom => custom.clone(),
        VoiceProfile::Headset => headset_preset(),
        VoiceProfile::Speaker => speaker_preset(),
        VoiceProfile::Auto => preset_for(output),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::dsp::NsLevel;

    #[test]
    fn presets_differ_only_on_aec() {
        assert!(!headset_preset().echo_cancel);
        assert!(speaker_preset().echo_cancel);
        assert!(headset_preset().agc && speaker_preset().agc);
        assert_eq!(headset_preset().noise_suppression, NsLevel::Moderate);
    }

    #[test]
    fn custom_passes_through_untouched() {
        let custom = DspConfig {
            echo_cancel: false,
            noise_suppression: NsLevel::Off,
            agc: true,
            vad: false,
            high_pass: false,
        };
        let got = effective(VoiceProfile::Custom, &custom, OutputKind::Speakers);
        assert_eq!(got.agc, true);
        assert_eq!(got.echo_cancel, false); // ignores the Speakers classification
    }

    #[test]
    fn auto_resolves_by_output_kind() {
        let custom = headset_preset();
        assert!(!effective(VoiceProfile::Auto, &custom, OutputKind::Headphones).echo_cancel);
        assert!(effective(VoiceProfile::Auto, &custom, OutputKind::Speakers).echo_cancel);
        // Unknown is the safe low-latency default = Headset (AEC off).
        assert!(!effective(VoiceProfile::Auto, &custom, OutputKind::Unknown).echo_cancel);
    }

    #[test]
    fn explicit_presets_ignore_classification() {
        let custom = DspConfig {
            echo_cancel: false, noise_suppression: NsLevel::Off,
            agc: false, vad: false, high_pass: false,
        };
        assert!(!effective(VoiceProfile::Headset, &custom, OutputKind::Speakers).echo_cancel);
        assert!(effective(VoiceProfile::Speaker, &custom, OutputKind::Headphones).echo_cancel);
    }
}
