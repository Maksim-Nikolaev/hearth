/// GStreamer sub-pipeline that captures the screen and outputs system-memory
/// video frames (ready for an encoder). Selected per OS.
pub fn capture_chain() -> &'static str {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_chain_is_present_and_converts() {
        let chain = capture_chain();

        assert!(!chain.is_empty());
        assert!(chain.contains("videoconvert"));
    }
}
