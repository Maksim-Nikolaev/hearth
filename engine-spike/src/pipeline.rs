use anyhow::Result;
use gstreamer as gst;
use gstreamer::prelude::*;

/// Single-process pipeline: capture + HW encode + decode + display on one machine.
/// Isolates the capture/encode half from the network half.
pub fn run_local() -> Result<()> {
    let encoder = crate::encoders::detect().0.unwrap_or("x265enc");

    let desc = format!(
        "ximagesrc use-damage=false ! videoconvert ! {encoder} ! h265parse ! avdec_h265 ! videoconvert ! autovideosink sync=false"
    );
    println!("pipeline: {desc}");

    let pipeline = gst::parse::launch(&desc)?;
    pipeline.set_state(gst::State::Playing)?;

    let bus = pipeline.bus().unwrap();

    for msg in bus.iter_timed(gst::ClockTime::NONE) {
        use gst::MessageView::*;

        match msg.view() {
            Eos(_) => break,
            Error(e) => {
                eprintln!("error: {} ({:?})", e.error(), e.debug());
                break;
            }
            _ => {}
        }
    }

    pipeline.set_state(gst::State::Null)?;

    Ok(())
}
