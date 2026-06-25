//! WebRTC AEC3 echo canceller for the native voice path — an alternative to the
//! speex canceller (`speex_aec`). Wraps `webrtc-audio-processing` with **only**
//! echo cancellation enabled (noise suppression / AGC stay with nnnoiseless and
//! our own AGC). Linux/macOS only: the bundled `webrtc-audio-processing` C++
//! build is Unix-only, so on Windows the native path always uses speex.
//!
//! Processes fixed 10 ms / 48 kHz mono frames (480 samples). Call [`cancel`] with
//! the far-end (playback) frame and the mic frame each cycle.

use anyhow::Result;
use webrtc_audio_processing::{
    Config, EchoCancellation, EchoCancellationSuppressionLevel, InitializationConfig, Processor,
};

pub struct WebrtcAec {
    processor: Processor,
}

impl WebrtcAec {
    /// `strength` (0–100) selects the suppression aggressiveness.
    pub fn new(strength: u8) -> Result<Self> {
        let processor = Processor::new(&InitializationConfig {
            num_capture_channels: 1,
            num_render_channels: 1,
            ..Default::default()
        })?;

        let mut aec = Self { processor };
        aec.set_strength(strength);

        Ok(aec)
    }

    /// Map 0–100 onto AEC3's three suppression levels and reconfigure. Live.
    pub fn set_strength(&mut self, strength: u8) {
        let suppression_level = if strength < 34 {
            EchoCancellationSuppressionLevel::Low
        } else if strength < 67 {
            EchoCancellationSuppressionLevel::Moderate
        } else {
            EchoCancellationSuppressionLevel::High
        };

        self.processor.set_config(Config {
            echo_cancellation: Some(EchoCancellation {
                suppression_level,
                stream_delay_ms: None,
                enable_delay_agnostic: true,
                enable_extended_filter: true,
            }),
            ..Config::default()
        });
    }

    /// Cancel echo on one 480-sample mic frame in place, using `far` (the 480
    /// rendered playback samples) as the reference. Both are mono f32.
    pub fn cancel(&mut self, mic: &mut [f32], far: &mut [f32]) {
        let _ = self.processor.process_render_frame(far);
        let _ = self.processor.process_capture_frame(mic);
    }
}
