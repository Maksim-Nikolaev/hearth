use anyhow::Result;

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

/// Default residual-echo suppression strength (0–100) for the native speex AEC.
/// Gentle enough to keep the voice natural; the Settings slider overrides it.
pub const DEFAULT_ECHO_STRENGTH: u8 = 40;

/// Which echo canceller the native voice path runs when `echo_cancel` is on.
/// `Webrtc` is unavailable on Windows (the bundled `webrtc-audio-processing`
/// build is Unix-only) and falls back to `Speex` there.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AecMethod {
    #[default]
    Speex,
    Webrtc,
}

/// Runtime-configurable DSP settings.
#[derive(Debug, Clone)]
pub struct DspConfig {
    pub echo_cancel: bool,
    /// Echo canceller to use on the native path when `echo_cancel` is on.
    pub aec_method: AecMethod,
    /// Residual-echo suppression strength (0–100) for the native speex AEC.
    /// Ignored by the GStreamer/webrtc DSP path.
    pub echo_cancel_strength: u8,
    pub noise_suppression: NsLevel,
    pub agc: bool,
    pub vad: bool,
    pub high_pass: bool,
}

// The real DSP wraps `webrtc-audio-processing`, whose bundled build uses
// autotools (libtoolize/automake/autoconf/configure/make) and only links under
// a Unix toolchain. On Windows (MSVC) that build is unavailable, so we fall back
// to a passthrough. The intended Windows replacement is GStreamer's `webrtcdsp`
// element (same upstream library), wired into the audio pipeline — TODO.
#[cfg(not(target_os = "windows"))]
pub use real::Dsp;
#[cfg(target_os = "windows")]
pub use stub::Dsp;

/// DSP pipeline wrapping `webrtc-audio-processing` (Linux/macOS).
///
/// Processes fixed 10 ms / 48 kHz mono frames (480 samples). Call
/// `process_render` with far-end (playback) audio before `process_capture`
/// with the microphone frame so AEC has a reference signal.
#[cfg(not(target_os = "windows"))]
mod real {
    use super::{DspConfig, NsLevel, Result, FRAME_SAMPLES};
    use webrtc_audio_processing::{
        Config, EchoCancellation, EchoCancellationSuppressionLevel, GainControl, GainControlMode,
        InitializationConfig, NoiseSuppression, NoiseSuppressionLevel, Processor, VoiceDetection,
        VoiceDetectionLikelihood,
    };

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
}

/// Passthrough DSP for Windows: no AEC / NS / AGC / VAD yet (the bundled
/// `webrtc-audio-processing` C++ build is Unix-only). Audio flows through
/// untouched and every frame is reported as voiced so VAD-gated capture never
/// drops the mic. Replace with GStreamer `webrtcdsp` for real Windows AEC.
#[cfg(target_os = "windows")]
mod stub {
    use super::{DspConfig, Result, FRAME_SAMPLES};

    pub struct Dsp {
        _private: (),
    }

    impl Dsp {
        pub fn new() -> Result<Dsp> {
            Ok(Dsp { _private: () })
        }

        pub fn set_config(&mut self, _cfg: &DspConfig) {}

        pub fn process_render(&mut self, frame: &mut [f32]) {
            debug_assert_eq!(frame.len(), FRAME_SAMPLES);
        }

        /// Passthrough: report voiced so VAD-gated capture stays open.
        pub fn process_capture(&mut self, frame: &mut [f32]) -> bool {
            debug_assert_eq!(frame.len(), FRAME_SAMPLES);
            true
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // VAD semantics are only provided by the real (non-Windows) DSP; the
    // Windows passthrough reports every frame as voiced.
    #[cfg(not(target_os = "windows"))]
    #[test]
    fn processes_a_silent_frame() {
        let mut dsp = Dsp::new().expect("create dsp");
        dsp.set_config(&DspConfig {
            echo_cancel: true,
            aec_method: AecMethod::Speex,
            echo_cancel_strength: DEFAULT_ECHO_STRENGTH,
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
                aec_method: AecMethod::Speex,
                echo_cancel_strength: DEFAULT_ECHO_STRENGTH,
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
