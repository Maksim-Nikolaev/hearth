/// GStreamer sub-pipeline that captures the screen and outputs system-memory
/// video frames (ready for an encoder).
///
/// `HEARTH_CAPTURE` overrides the chain entirely, so a platform whose element
/// names differ from our defaults (notably a Windows GStreamer build) can be
/// adapted on-site without recompiling. It also doubles as the bench hook, e.g.
/// `HEARTH_CAPTURE="videotestsrc is-live=true pattern=ball ! timeoverlay ! videoconvert"`.
pub fn capture_chain() -> String {
    if let Ok(custom) = std::env::var("HEARTH_CAPTURE") {
        if !custom.trim().is_empty() {
            return custom;
        }
    }

    default_capture_chain().to_string()
}

fn default_capture_chain() -> &'static str {
    #[cfg(target_os = "linux")]
    {
        "ximagesrc use-damage=false ! videoconvert"
    }
    #[cfg(target_os = "windows")]
    {
        // d3d11screencapturesrc yields GPU memory; download to system memory for the encoder.
        "d3d11screencapturesrc ! d3d11download ! videoconvert"
    }
    #[cfg(not(any(target_os = "linux", target_os = "windows")))]
    {
        "videotestsrc ! videoconvert"
    }
}

/// Raw-video caps enforced after capture so both peers and the latency/bitrate
/// measurements share a known format. Framerate is always pinned (default 30);
/// width and height are pinned only when both `HEARTH_WIDTH` and `HEARTH_HEIGHT`
/// are set, otherwise the native capture size passes through.
pub fn video_caps() -> String {
    let fps = env_u32("HEARTH_FPS", 30);

    match (env_opt_u32("HEARTH_WIDTH"), env_opt_u32("HEARTH_HEIGHT")) {
        (Some(w), Some(h)) => format!("video/x-raw,width={w},height={h},framerate={fps}/1"),
        _ => format!("video/x-raw,framerate={fps}/1"),
    }
}

fn env_opt_u32(key: &str) -> Option<u32> {
    std::env::var(key).ok().and_then(|v| v.trim().parse().ok())
}

fn env_u32(key: &str, default: u32) -> u32 {
    env_opt_u32(key).unwrap_or(default)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_chain_is_present_and_converts() {
        let chain = default_capture_chain();

        assert!(!chain.is_empty());
        assert!(chain.contains("videoconvert"));
    }

    #[test]
    fn video_caps_always_pins_framerate() {
        let caps = video_caps();

        assert!(caps.starts_with("video/x-raw"));
        assert!(caps.contains("framerate="));
    }
}
