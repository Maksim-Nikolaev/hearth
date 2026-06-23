use super::sources::{ContentType, ShareConfig, ShareSource};

/// Build the GStreamer source sub-pipeline string for the given config.
///
/// `HEARTH_CAPTURE` overrides everything – if set, the env value is returned
/// unchanged. This preserves the testing hook used across the whole project.
///
/// Otherwise the chain is constructed from `cfg`:
/// - `Screen { monitor }` → `ximagesrc use-damage=false`
/// - `Window { xid }` → `ximagesrc use-damage=false xid=0x…`
///
/// Followed by `videoconvert ! videoscale ! capsfilter` with the resolved
/// width, height, and fps (capped at 15 when `ContentType::Clarity`).
pub fn capture_chain(cfg: &ShareConfig) -> String {
    // HEARTH_CAPTURE is the highest-priority override – delegate to the
    // existing capture module so the logic lives in exactly one place.
    if let Ok(custom) = std::env::var("HEARTH_CAPTURE") {
        if !custom.trim().is_empty() {
            return custom;
        }
    }

    default_chain(cfg)
}

fn default_chain(cfg: &ShareConfig) -> String {
    let src = source_element(&cfg.source);

    let fps = effective_fps(cfg);

    // `videorate` adapts the live capture to the target fps. The trailing queue
    // decouples `ximagesrc` from the downstream encoder/sink (without it a live
    // source feeding `gtk4paintablesink` goes black). `leaky=downstream` drops
    // the oldest buffered frame when full (newest-wins, lowest latency) so
    // back-pressure never reaches the X grabber. Bounded to 3 buffers;
    // `max-size-bytes=0`/`max-size-time=0` disable those limits because a
    // single 4K frame exceeds the 10 MB default, which would misbehave.
    format!(
        "{src} ! videoconvert ! videorate ! videoscale \
         ! video/x-raw,width={w},height={h},framerate={fps}/1 \
         ! queue leaky=downstream max-size-buffers=3 max-size-bytes=0 max-size-time=0",
        w = cfg.width,
        h = cfg.height,
    )
}

/// The platform capture source element, emitting system-memory video ready for
/// the shared `videoconvert ! videorate ! videoscale` tail.
#[cfg(target_os = "linux")]
fn source_element(source: &ShareSource) -> String {
    match source {
        ShareSource::Screen { .. } => "ximagesrc use-damage=false".to_string(),
        ShareSource::Window { xid } => format!("ximagesrc use-damage=false xid=0x{xid:x}"),
    }
}

/// Windows: `d3d11screencapturesrc` yields GPU (D3D11) memory, so download to
/// system memory before the shared `videoconvert` tail. Per-window capture is
/// not wired yet (no window enumeration on Windows), so both source variants
/// capture a full monitor.
#[cfg(target_os = "windows")]
fn source_element(source: &ShareSource) -> String {
    match source {
        ShareSource::Screen { monitor } => {
            format!("d3d11screencapturesrc monitor-index={monitor} ! d3d11download")
        },
        ShareSource::Window { .. } => "d3d11screencapturesrc ! d3d11download".to_string(),
    }
}

#[cfg(not(any(target_os = "linux", target_os = "windows")))]
fn source_element(_source: &ShareSource) -> String {
    "videotestsrc is-live=true".to_string()
}

/// Resolve the actual fps ceiling: `Clarity` caps at 15, `Smoothness` keeps
/// the configured value.
fn effective_fps(cfg: &ShareConfig) -> u32 {
    match cfg.content {
        ContentType::Smoothness => cfg.fps,
        ContentType::Clarity => cfg.fps.min(15),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::screen::sources::{ContentType, ShareConfig, ShareSource};

    // Serializes all tests that read or write HEARTH_CAPTURE so parallel `cargo
    // test` runs can't observe each other's transient env state. Every test that
    // calls `capture_chain` must hold this (not just the writer), or it may read
    // the writer's transient value. Poison-tolerant: a failing assertion in one
    // test must not cascade-panic the others.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn env_guard() -> std::sync::MutexGuard<'static, ()> {
        ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The capture source element for the current platform (used by the
    /// platform-agnostic chain-shape assertions below).
    #[cfg(target_os = "linux")]
    const PLATFORM_SRC: &str = "ximagesrc";
    #[cfg(target_os = "windows")]
    const PLATFORM_SRC: &str = "d3d11screencapturesrc";
    #[cfg(not(any(target_os = "linux", target_os = "windows")))]
    const PLATFORM_SRC: &str = "videotestsrc";

    #[test]
    fn screen_chain_uses_platform_source_and_caps() {
        let _guard = env_guard();
        let cfg = ShareConfig {
            source: ShareSource::Screen { monitor: 0 },
            width: 1920,
            height: 1080,
            fps: 60,
            content: ContentType::Smoothness,
            ..Default::default()
        };
        let chain = capture_chain(&cfg);

        assert!(chain.contains(PLATFORM_SRC));
        assert!(chain.contains("framerate=60/1"));
        assert!(chain.contains("1920") && chain.contains("1080"));
        assert!(chain.contains("leaky=downstream"));
        assert!(chain.contains("max-size-buffers=3"));
        assert!(chain.contains("max-size-bytes=0"));
        assert!(chain.contains("max-size-time=0"));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn window_chain_sets_xid() {
        let _guard = env_guard();
        let cfg = ShareConfig {
            source: ShareSource::Window { xid: 0x1400003 },
            width: 1280,
            height: 720,
            fps: 30,
            content: ContentType::Clarity,
            ..Default::default()
        };
        let chain = capture_chain(&cfg);

        assert!(chain.contains("xid=0x1400003"));
    }

    #[test]
    fn clarity_caps_fps_at_15() {
        let _guard = env_guard();
        let cfg = ShareConfig {
            source: ShareSource::Screen { monitor: 0 },
            width: 1920,
            height: 1080,
            fps: 60,
            content: ContentType::Clarity,
            ..Default::default()
        };
        let chain = capture_chain(&cfg);

        assert!(chain.contains("framerate=15/1"));
        assert!(!chain.contains("framerate=60/1"));
    }

    #[test]
    fn smoothness_keeps_configured_fps() {
        let _guard = env_guard();
        let cfg = ShareConfig {
            source: ShareSource::Screen { monitor: 0 },
            width: 1280,
            height: 720,
            fps: 30,
            content: ContentType::Smoothness,
            ..Default::default()
        };
        let chain = capture_chain(&cfg);

        assert!(chain.contains("framerate=30/1"));
    }

    #[test]
    fn env_override_bypasses_config() {
        let _guard = env_guard();

        std::env::set_var("HEARTH_CAPTURE", "videotestsrc is-live=true pattern=ball");

        let cfg = ShareConfig::default();
        let chain = capture_chain(&cfg);

        std::env::remove_var("HEARTH_CAPTURE");

        assert_eq!(chain, "videotestsrc is-live=true pattern=ball");
        assert!(!chain.contains("ximagesrc"));
    }
}
