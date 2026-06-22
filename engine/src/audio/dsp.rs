use anyhow::Result;
use webrtc_audio_processing::{
    Config, EchoCancellation, EchoCancellationSuppressionLevel, GainControl, GainControlMode,
    InitializationConfig, NoiseSuppression, NoiseSuppressionLevel, Processor, VoiceDetection,
    VoiceDetectionLikelihood,
};

/// 10 ms at 48 kHz, mono. The crate requires exactly this frame size.
pub const FRAME_SAMPLES: usize = 480;

/// Noise suppression aggressiveness level.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NsLevel {
    Off,
    Low,
    Moderate,
    High,
}

/// Runtime-configurable DSP settings.
#[derive(Debug, Clone)]
pub struct DspConfig {
    pub echo_cancel: bool,
    pub noise_suppression: NsLevel,
    pub agc: bool,
    pub vad: bool,
    pub high_pass: bool,
}

/// DSP pipeline wrapping `webrtc-audio-processing`.
///
/// Processes fixed 10 ms / 48 kHz mono frames (480 samples). Call
/// `process_render` with far-end (playback) audio before `process_capture`
/// with the microphone frame so AEC has a reference signal.
pub struct Dsp {
    processor: Processor,
}

impl Dsp {
    pub fn new() -> Result<Dsp> {
        let processor = Processor::new(&InitializationConfig {
            num_capture_channels: 1,
            num_render_channels: 1,
            ..Default::default()
        })?;

        Ok(Dsp { processor })
    }

    /// Apply a new configuration immediately; safe to call between frames.
    pub fn set_config(&mut self, cfg: &DspConfig) {
        let config = Config {
            echo_cancellation: cfg.echo_cancel.then(|| EchoCancellation {
                suppression_level: EchoCancellationSuppressionLevel::High,
                stream_delay_ms: None,
                enable_delay_agnostic: true,
                enable_extended_filter: true,
            }),
            noise_suppression: match cfg.noise_suppression {
                NsLevel::Off => None,
                NsLevel::Low => {
                    Some(NoiseSuppression { suppression_level: NoiseSuppressionLevel::Low })
                },
                NsLevel::Moderate => {
                    Some(NoiseSuppression { suppression_level: NoiseSuppressionLevel::Moderate })
                },
                NsLevel::High => {
                    Some(NoiseSuppression { suppression_level: NoiseSuppressionLevel::High })
                },
            },
            gain_control: cfg.agc.then(|| GainControl {
                mode: GainControlMode::AdaptiveDigital,
                target_level_dbfs: 3,
                compression_gain_db: 9,
                enable_limiter: true,
            }),
            voice_detection: cfg.vad.then(|| VoiceDetection {
                detection_likelihood: VoiceDetectionLikelihood::Moderate,
            }),
            enable_high_pass_filter: cfg.high_pass,
            ..Config::default()
        };

        self.processor.set_config(config);
    }

    /// Process one 10 ms render (far-end/playback) frame so AEC has a reference.
    pub fn process_render(&mut self, frame: &mut [f32]) {
        debug_assert_eq!(frame.len(), FRAME_SAMPLES);
        let _ = self.processor.process_render_frame(frame);
    }

    /// Process one 10 ms capture (mic) frame in place; returns the VAD voice flag.
    pub fn process_capture(&mut self, frame: &mut [f32]) -> bool {
        debug_assert_eq!(frame.len(), FRAME_SAMPLES);

        if self.processor.process_capture_frame(frame).is_err() {
            return false;
        }

        self.processor.get_stats().has_voice.unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn processes_a_silent_frame() {
        let mut dsp = Dsp::new().expect("create dsp");
        dsp.set_config(&DspConfig {
            echo_cancel: true,
            noise_suppression: NsLevel::High,
            agc: true,
            vad: true,
            high_pass: true,
        });

        let mut render = vec![0.0f32; FRAME_SAMPLES];
        dsp.process_render(&mut render);

        let mut capture = vec![0.0f32; FRAME_SAMPLES];
        let voice = dsp.process_capture(&mut capture);
        assert!(!voice, "silence must not be detected as voice");
    }

    #[test]
    fn config_toggles_apply_without_error() {
        let mut dsp = Dsp::new().unwrap();
        for ns in [NsLevel::Off, NsLevel::Low, NsLevel::Moderate, NsLevel::High] {
            dsp.set_config(&DspConfig {
                echo_cancel: false,
                noise_suppression: ns,
                agc: false,
                vad: false,
                high_pass: false,
            });
            let mut f = vec![0.0f32; FRAME_SAMPLES];
            let _ = dsp.process_capture(&mut f);
        }
    }
}
