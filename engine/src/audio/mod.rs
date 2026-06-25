pub mod capture;
pub mod classify;
pub mod devices;
pub mod dsp;
pub mod gate;

// Receive-side dejitter / reorder buffer for the native voice transport.
#[cfg(any(target_os = "windows", target_os = "linux"))]
pub mod jitter;

pub mod monitor;
pub mod profile;
pub mod rt;

// Device-independent voice-path microbench (Opus + UDP round-trip). Cross-platform.
pub mod voicebench;

// Phase 2 spike: WASAPI IAudioClient3 low-latency shared-mode loopback, to
// measure the device floor the GStreamer `wasapi2` path can't reach.
#[cfg(target_os = "windows")]
pub mod wasapi3;

// Native low-latency voice device I/O: WASAPI on Windows, PipeWire on Linux.
// Replaces GStreamer wasapi2/pulsesrc on the voice path.
#[cfg(any(target_os = "windows", target_os = "linux"))]
pub mod native;

// Native voice transport (NativeCapture+Opus+UDP+NativePlayback). Platform-
// independent; rides whichever device backend `native` selects per target.
#[cfg(any(target_os = "windows", target_os = "linux"))]
pub mod native_voice;

// Tuned speexdsp echo canceller used by the native voice path.
#[cfg(any(target_os = "windows", target_os = "linux"))]
pub mod speex_aec;

// Alternative WebRTC AEC3 canceller for the native path (Unix-only build).
#[cfg(not(target_os = "windows"))]
pub mod webrtc_aec;

/// Microphone capture source element for this platform. Linux/macOS use
/// PulseAudio (`pulsesrc`); Windows uses WASAPI (`wasapi2src`). Both accept a
/// `device` property whose value is the id returned by [`devices::list_devices`].
pub(crate) fn capture_src_factory() -> &'static str {
    #[cfg(target_os = "windows")]
    {
        "wasapi2src"
    }
    #[cfg(not(target_os = "windows"))]
    {
        "pulsesrc"
    }
}

/// Playback sink element for this platform (used by the mic-test monitor). The
/// live call path uses `autoaudiosink`, which already selects the right sink.
pub(crate) fn playback_sink_factory() -> &'static str {
    #[cfg(target_os = "windows")]
    {
        "wasapi2sink"
    }
    #[cfg(not(target_os = "windows"))]
    {
        "pulsesink"
    }
}
