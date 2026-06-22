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
    let src = match cfg.source {
        ShareSource::Screen { .. } => "ximagesrc use-damage=false".to_string(),
        ShareSource::Window { xid } => format!("ximagesrc use-damage=false xid=0x{xid:x}"),
    };

    let fps = effective_fps(cfg);

    // `videorate` adapts the live capture to the target fps, and the trailing
    // `queue` decouples the live `ximagesrc` thread from the downstream sink /
    // encoder (a live source feeding `gtk4paintablesink` directly stalls without
    // it – the cause of the black preview/share with a real screen).
    format!(
        "{src} ! videoconvert ! videorate ! videoscale \
         ! video/x-raw,width={w},height={h},framerate={fps}/1 ! queue",
        w = cfg.width,
        h = cfg.height,
    )
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

    #[test]
    fn screen_chain_uses_ximagesrc_and_caps() {
        let cfg = ShareConfig {
            source: ShareSource::Screen { monitor: 0 },
            width: 1920,
            height: 1080,
            fps: 60,
            content: ContentType::Smoothness,
            ..Default::default()
        };
        let chain = capture_chain(&cfg);

        assert!(chain.contains("ximagesrc"));
        assert!(chain.contains("framerate=60/1"));
        assert!(chain.contains("1920") && chain.contains("1080"));
    }

    #[test]
    fn window_chain_sets_xid() {
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
        std::env::set_var("HEARTH_CAPTURE", "videotestsrc is-live=true pattern=ball");

        let cfg = ShareConfig::default();
        let chain = capture_chain(&cfg);

        std::env::remove_var("HEARTH_CAPTURE");

        assert_eq!(chain, "videotestsrc is-live=true pattern=ball");
        assert!(!chain.contains("ximagesrc"));
    }
}
