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

/// X11 CPU capture via `ximagesrc`. Delegates to the existing `capture_chain`
/// (which also honours the `HEARTH_CAPTURE` test override).
pub struct X11Ximage;

impl CaptureBackend for X11Ximage {
    fn name(&self) -> &'static str {
        "x11-ximage"
    }

    fn capture_chain(&self, cfg: &ShareConfig) -> String {
        super::capture::capture_chain(cfg)
    }
}

/// Pick the best capture backend for this platform/display server. Only the X11
/// ximagesrc backend is wired today; the match arms mark where Wayland/Windows/
/// macOS/X11-GPU backends slot in. Compile-time OS gating via cfg!, runtime
/// display-server probing (e.g. XDG_SESSION_TYPE) once more backends exist.
pub fn detect_capture_backend() -> Box<dyn CaptureBackend> {
    // TODO(wayland): if XDG_SESSION_TYPE == "wayland" -> Pipewire portal backend.
    // TODO(x11-gpu): xcomposite+EGLImage backend (M8 Phase 2) when proven.
    let backend: Box<dyn CaptureBackend> = Box::new(X11Ximage);

    eprintln!("capture backend: {}", backend.name());

    backend
}
