use super::sources::ShareSource;

/// Build the GStreamer pipeline string for a one-shot thumbnail capture.
///
/// Respects `HEARTH_CAPTURE`: if set, appends a single-frame limit to the
/// override source rather than using `ximagesrc`.
pub fn thumbnail_pipeline(source: &ShareSource, max_w: u32, max_h: u32) -> String {
    let src = if let Ok(custom) = std::env::var("HEARTH_CAPTURE") {
        if !custom.trim().is_empty() {
            custom
        } else {
            default_src(source)
        }
    } else {
        default_src(source)
    };

    format!(
        "{src} ! videoconvert ! videoscale \
         ! video/x-raw,width=(int){max_w},height=(int){max_h} \
         ! videoconvert ! pngenc ! appsink name=sink max-buffers=1 drop=true"
    )
}

fn default_src(source: &ShareSource) -> String {
    match source {
        ShareSource::Screen { .. } => {
            "ximagesrc use-damage=false num-buffers=1".to_string()
        }
        ShareSource::Window { xid } => {
            format!("ximagesrc use-damage=false xid=0x{xid:x} num-buffers=1")
        }
    }
}

/// Capture a single frame from `source` and return it as PNG bytes.
///
/// Uses a one-shot GStreamer pipeline (`num-buffers=1`) so it is not a live
/// pipeline – it captures exactly one frame and tears down. Returns `None` on
/// any failure (caller shows a placeholder).
pub fn thumbnail(source: &ShareSource, max_w: u32, max_h: u32) -> Option<Vec<u8>> {
    use gstreamer::prelude::*;
    use gstreamer_app::AppSink;

    gstreamer::init().ok()?;

    let pipeline_str = thumbnail_pipeline(source, max_w, max_h);

    let pipeline = gstreamer::parse::launch(&pipeline_str).ok()?;
    let pipeline = pipeline.downcast::<gstreamer::Pipeline>().ok()?;

    let sink = pipeline.by_name("sink")?;
    let appsink = sink.downcast::<AppSink>().ok()?;

    pipeline.set_state(gstreamer::State::Playing).ok()?;

    // Pull one sample with a short timeout (3 s is generous for a static frame).
    let timeout = gstreamer::ClockTime::from_seconds(3);
    let sample = appsink.try_pull_sample(timeout)?;

    let buffer = sample.buffer()?;
    let map = buffer.map_readable().ok()?;
    let bytes = map.as_slice().to_vec();

    let _ = pipeline.set_state(gstreamer::State::Null);

    Some(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::screen::sources::ShareSource;

    #[test]
    fn screen_thumbnail_pipeline_contains_pngenc_and_appsink() {
        let src = ShareSource::Screen { monitor: 0 };
        let s = thumbnail_pipeline(&src, 240, 135);

        assert!(s.contains("pngenc"), "pipeline must encode to PNG");
        assert!(s.contains("appsink"), "pipeline must end with appsink");
        assert!(s.contains("240") && s.contains("135"), "pipeline must include requested dimensions");
    }

    #[test]
    fn window_thumbnail_pipeline_sets_xid() {
        let src = ShareSource::Window { xid: 0xdeadbeef };
        let s = thumbnail_pipeline(&src, 240, 135);

        assert!(s.contains("xid=0xdeadbeef"));
    }

    #[test]
    fn hearth_capture_override_replaces_source() {
        std::env::set_var("HEARTH_CAPTURE", "videotestsrc num-buffers=1 pattern=smpte");

        let src = ShareSource::Screen { monitor: 0 };
        let s = thumbnail_pipeline(&src, 240, 135);

        std::env::remove_var("HEARTH_CAPTURE");

        assert!(s.contains("videotestsrc"), "override source must appear");
        assert!(!s.contains("ximagesrc"), "ximagesrc must NOT appear when override is set");
        assert!(s.contains("pngenc"), "pipeline must still encode to PNG");
    }
}
