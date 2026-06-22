use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use gstreamer as gst;
use gstreamer::glib;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use uuid::Uuid;

use crate::encoders;
use crate::flow_peer::tune_encoder;
use crate::screen::backend::detect_capture_backend;
use crate::screen::sources::ShareConfig;

/// One capture+encode+preview pipeline. Encoded H265 is fanned out to every
/// registered viewer `AppSrc` from the single appsink callback; the same raw
/// frames are tee'd to a `gtk4paintablesink` for local preview.
///
/// When `encode == false` (preview-only mode) the encoder branch is omitted:
/// no H265 frames are produced and no CPU/GPU encode budget is consumed.
pub struct ScreenSource {
    pipeline: gst::Pipeline,
    paintable: glib::Object,
    viewers: Arc<Mutex<HashMap<Uuid, gst_app::AppSrc>>>,
}

impl ScreenSource {
    /// Build the pipeline and set it to PLAYING.
    ///
    /// Returns `None` when a required element is unavailable or the pipeline
    /// refuses to start (e.g. in a headless environment without a display),
    /// matching the non-fatal convention of the old `build_preview_pipeline`.
    pub fn new(cfg: &ShareConfig, encode: bool) -> Option<Self> {
        gst::init().ok()?;

        let backend = detect_capture_backend();
        let chain_str = backend.capture_chain(cfg);

        let src = gst::parse::bin_from_description(&chain_str, true).ok()?;

        let tee = gst::ElementFactory::make("tee").build().ok()?;

        // Preview branch: always present so the local picker can show a frame.
        let prev_q = gst::ElementFactory::make("queue")
            .property_from_str("leaky", "downstream")
            .property("max-size-buffers", 3u32)
            .property("max-size-bytes", 0u32)
            .property("max-size-time", 0u64)
            .build()
            .ok()?;
        let preview_sink = gst::ElementFactory::make("gtk4paintablesink").build().ok()?;
        let paintable: glib::Object = preview_sink.property("paintable");

        let viewers: Arc<Mutex<HashMap<Uuid, gst_app::AppSrc>>> =
            Arc::new(Mutex::new(HashMap::new()));

        let pipeline = gst::Pipeline::new();
        pipeline
            .add_many([src.upcast_ref::<gst::Element>(), &tee, &prev_q, &preview_sink])
            .ok()?;

        gst::Element::link_many([src.upcast_ref::<gst::Element>(), &tee]).ok()?;

        // tee → preview queue → gtk4paintablesink
        tee.link(&prev_q).ok()?;
        prev_q.link(&preview_sink).ok()?;

        if encode {
            // Encode branch: tee → queue → encoder → h265parse → appsink
            let enc_q = gst::ElementFactory::make("queue")
                .property_from_str("leaky", "downstream")
                .property("max-size-buffers", 3u32)
                .property("max-size-bytes", 0u32)
                .property("max-size-time", 0u64)
                .build()
                .ok()?;

            let encoder_name = encoders::detect().0.unwrap_or("x265enc");
            let enc = gst::ElementFactory::make(encoder_name).build().ok()?;
            tune_encoder(&enc, cfg.bitrate_kbps);

            let parse = gst::ElementFactory::make("h265parse")
                .property("config-interval", -1i32)
                .build()
                .ok()?;

            let h265_caps = gst::Caps::builder("video/x-h265")
                .field("stream-format", "byte-stream")
                .field("alignment", "au")
                .build();

            let appsink = gst_app::AppSink::builder()
                .caps(&h265_caps)
                .sync(false)
                .drop(true)
                .max_buffers(3)
                .build();

            pipeline
                .add_many([&enc_q, &enc, &parse, appsink.upcast_ref()])
                .ok()?;

            tee.link(&enc_q).ok()?;
            gst::Element::link_many([&enc_q, &enc, &parse, appsink.upcast_ref()]).ok()?;

            // Fan-out callback: runs on the appsink streaming thread.
            let viewers_cb = viewers.clone();
            let callbacks = gst_app::AppSinkCallbacks::builder()
                .new_sample(move |sink| {
                    let sample = sink.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                    let buffer = sample.buffer().ok_or(gst::FlowError::Error)?;

                    let guard = viewers_cb.lock().unwrap();
                    for appsrc in guard.values() {
                        // copy() is a shallow refcount-sharing copy; each appsrc
                        // needs its own ref so they can release independently.
                        let _ = appsrc.push_buffer(buffer.copy());
                    }

                    Ok(gst::FlowSuccess::Ok)
                })
                .build();
            appsink.set_callbacks(callbacks);
        }

        pipeline.set_state(gst::State::Playing).ok()?;

        Some(Self { pipeline, paintable, viewers })
    }

    /// Register a viewer appsrc to receive encoded H265 buffers.
    pub fn register_viewer(&self, id: Uuid, src: gst_app::AppSrc) {
        self.viewers.lock().unwrap().insert(id, src);
    }

    /// Remove a viewer appsrc. No-op when the id is not registered.
    pub fn unregister_viewer(&self, id: &Uuid) {
        self.viewers.lock().unwrap().remove(id);
    }

    /// The local preview paintable (`gdk::Paintable` behind a `glib::Object`).
    pub fn paintable(&self) -> glib::Object {
        self.paintable.clone()
    }

    /// Tear down the pipeline synchronously before drop to avoid the stop→start
    /// race where the next pipeline starts before this one fully releases resources.
    pub fn stop(self) {
        let _ = self.pipeline.set_state(gst::State::Null);
        let _ = self.pipeline.state(gst::ClockTime::from_seconds(2));
    }
}

impl Drop for ScreenSource {
    fn drop(&mut self) {
        let _ = self.pipeline.set_state(gst::State::Null);
        let _ = self.pipeline.state(gst::ClockTime::from_seconds(2));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Registry operations work correctly without any GStreamer pipeline.
    #[test]
    fn register_unregister_no_pipeline() {
        let viewers: Arc<Mutex<HashMap<Uuid, gst_app::AppSrc>>> =
            Arc::new(Mutex::new(HashMap::new()));

        let id = Uuid::from_u128(1);

        // Build a minimal appsrc for insertion (no pipeline needed for the type).
        gst::init().unwrap();
        let appsrc = gst_app::AppSrc::builder().build();

        viewers.lock().unwrap().insert(id, appsrc);
        assert_eq!(viewers.lock().unwrap().len(), 1);

        viewers.lock().unwrap().remove(&id);
        assert!(viewers.lock().unwrap().is_empty());
    }

    /// `ScreenSource::new` with a synthetic source must build without panicking
    /// when `gtk4paintablesink` and an encoder are available. Gated behind
    /// `HEARTH_CAPTURE` so CI (headless/no X11) skips the live-pipeline path.
    #[test]
    fn preview_only_new_with_synthetic_source() {
        let capture = std::env::var("HEARTH_CAPTURE").unwrap_or_default();
        if capture.is_empty() {
            // Skip: no synthetic source configured, real ximagesrc would need X11.
            return;
        }

        gst::init().unwrap();

        let cfg = ShareConfig::default();

        // Preview-only: must not build the encode branch.
        let source = ScreenSource::new(&cfg, false);

        // The result is None in fully headless environments (no gtk4paintablesink).
        // Either outcome is acceptable; the important thing is no panic.
        drop(source);
    }

    /// Same guard for the encode path: only runs when HEARTH_CAPTURE is set.
    #[test]
    fn encode_new_with_synthetic_source() {
        let capture = std::env::var("HEARTH_CAPTURE").unwrap_or_default();
        if capture.is_empty() {
            return;
        }

        gst::init().unwrap();

        let cfg = ShareConfig::default();
        let source = ScreenSource::new(&cfg, true);

        drop(source);
    }

    /// Tight stop→start cycle must not crash, hang, or trigger a GStreamer
    /// abort. Validates that synchronous teardown fully releases pipeline
    /// resources before the next pipeline is built on the same source.
    ///
    /// Gated behind `HEARTH_CAPTURE` (synthetic source) so headless CI skips
    /// the live-pipeline path. Run with:
    ///
    /// ```text
    /// HEARTH_CAPTURE="videotestsrc is-live=true num-buffers=10 pattern=ball \
    ///   ! videoconvert" cargo test -p engine screen::source -- --nocapture
    /// ```
    #[test]
    fn stop_start_cycle_no_crash() {
        let capture = std::env::var("HEARTH_CAPTURE").unwrap_or_default();
        if capture.is_empty() {
            return;
        }

        gst::init().unwrap();

        let cfg = ShareConfig::default();

        // Five rapid preview-only cycles.
        for _ in 0..5 {
            if let Some(source) = ScreenSource::new(&cfg, false) {
                std::thread::sleep(std::time::Duration::from_millis(50));
                source.stop();
            }
        }
    }

    /// Same cycle test alternating preview-only ↔ encode modes, mirroring the
    /// real start_preview → start_share → start_preview transition sequence.
    #[test]
    fn stop_start_cycle_alternating_encode_no_crash() {
        let capture = std::env::var("HEARTH_CAPTURE").unwrap_or_default();
        if capture.is_empty() {
            return;
        }

        gst::init().unwrap();

        let cfg = ShareConfig::default();

        for i in 0..5 {
            let encode = i % 2 == 1;

            if let Some(source) = ScreenSource::new(&cfg, encode) {
                std::thread::sleep(std::time::Duration::from_millis(50));
                source.stop();
            }
        }
    }
}
