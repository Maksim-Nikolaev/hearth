use super::sources::ShareConfig;

/// A platform/display-server screen-capture front-end. Produces the GStreamer
/// source+caps sub-pipeline (everything up to and including the trailing leaky
/// queue) that feeds the shared `tee` in `ScreenSource`. Implementations are
/// selected at runtime by `detect_capture_backend`, most-capable first, with
/// graceful fallback. New platforms (Wayland pipewiresrc, Windows WGC, macOS
/// ScreenCaptureKit) and the Phase-2 X11 xcomposite GPU source are added here
/// without touching the encode/fan-out/webrtc path.
pub trait CaptureBackend: Send + Sync {
    fn name(&self) -> &'static str;
    fn capture_chain(&self, cfg: &ShareConfig) -> String;
}

/// The default platform CPU/GPU capture. Delegates to `capture_chain`, which is
/// itself per-OS (`ximagesrc` on X11, `d3d11screencapturesrc` on Windows) and
/// honours the `HEARTH_CAPTURE` test override. The name reflects the OS so logs
/// aren't misleading.
pub struct PlatformCapture;

impl CaptureBackend for PlatformCapture {
    fn name(&self) -> &'static str {
        #[cfg(target_os = "windows")]
        {
            "windows-d3d11"
        }
        #[cfg(target_os = "linux")]
        {
            "x11-ximage"
        }
        #[cfg(not(any(target_os = "windows", target_os = "linux")))]
        {
            "videotestsrc"
        }
    }

    fn capture_chain(&self, cfg: &ShareConfig) -> String {
        super::capture::capture_chain(cfg)
    }
}

/// Pick the capture backend for this platform. The chain itself is per-OS
/// (`capture_chain`), so one `PlatformCapture` covers X11/Windows/fallback; the
/// match arms below mark where richer backends (Wayland pipewire portal, X11-GPU
/// xcomposite) slot in once proven.
pub fn detect_capture_backend() -> Box<dyn CaptureBackend> {
    // TODO(wayland): if XDG_SESSION_TYPE == "wayland" -> Pipewire portal backend.
    // TODO(x11-gpu): xcomposite+EGLImage backend (M8 Phase 2) when proven.
    let backend: Box<dyn CaptureBackend> = Box::new(PlatformCapture);

    eprintln!("capture backend: {}", backend.name());

    backend
}
