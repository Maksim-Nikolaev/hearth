pub mod capture;
pub mod devices;
pub mod dsp;
pub mod gate;
pub mod monitor;

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
