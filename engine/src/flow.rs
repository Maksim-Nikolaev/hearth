pub use hearth_protocol::Flow;

/// How an incoming video flow is displayed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum VideoSink {
    /// `autovideosink` – GStreamer opens its own window. Used by the CLI and as a
    /// headless-friendly default.
    #[default]
    Auto,
    /// `gtk4paintablesink` – exposes a `gdk::Paintable` for in-app embedding.
    /// Requires the gtk4 plugin on `GST_PLUGIN_PATH` at runtime.
    Paintable,
}
