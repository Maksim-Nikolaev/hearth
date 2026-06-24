pub mod capture;
pub mod classify;
pub mod devices;
pub mod dsp;
pub mod gate;
pub mod monitor;
pub mod profile;

// Device-independent voice-path microbench (Opus + UDP round-trip). Cross-platform.
pub mod voicebench;

// Phase 2 spike: WASAPI IAudioClient3 low-latency shared-mode loopback, to
// measure the device floor the GStreamer `wasapi2` path can't reach.
#[cfg(target_os = "windows")]
pub mod wasapi3;

// Phase 2: native WASAPI capture/playback (replaces GStreamer wasapi2 on voice).
#[cfg(target_os = "windows")]
pub mod native;

// Phase 2: native voice transport (NativeCapture+Opus+UDP+NativePlayback).
#[cfg(target_os = "windows")]
pub mod native_voice;

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
