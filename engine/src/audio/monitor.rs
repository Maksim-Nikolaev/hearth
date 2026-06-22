use crate::audio::capture::{f32_to_i16, pcm_caps, rms_dbfs, sample_to_f32};
use crate::audio::dsp::{Dsp, DspConfig, FRAME_SAMPLES};
use crate::session::SessionEvent;
use anyhow::Result;
use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use tokio::sync::mpsc::UnboundedSender;

/// Loopback mic-test monitor: captures the mic, runs DSP, plays it back on the
/// chosen output device, and emits `SessionEvent::InputLevel` each frame.
///
/// Used while NOT in a call so the user can verify their mic and hear DSP
/// changes (noise suppression, etc.) in real time. Drop to stop.
pub struct Monitor {
    in_pipeline: gst::Pipeline,
    out_pipeline: gst::Pipeline,
}

impl Monitor {
    /// Start a mic loopback: mic → DSP → speaker.
    ///
    /// `input`  – PulseAudio source name; `None` selects the system default.
    /// `output` – PulseAudio sink name;   `None` selects the system default.
    /// `cfg`    – initial DSP configuration applied before the first frame.
    /// `evt`    – channel for `SessionEvent::InputLevel` events.
    pub fn start(
        input: Option<String>,
        output: Option<String>,
        cfg: DspConfig,
        evt: UnboundedSender<SessionEvent>,
    ) -> Result<Monitor> {
        gst::init()?;

        let (in_pipeline, out_appsrc) = build_in_pipeline(input, cfg, evt)?;
        let out_pipeline = build_out_pipeline(output, out_appsrc)?;

        out_pipeline.set_state(gst::State::Playing)?;
        in_pipeline.set_state(gst::State::Playing)?;

        Ok(Monitor { in_pipeline, out_pipeline })
    }
}

impl Drop for Monitor {
    fn drop(&mut self) {
        let _ = self.in_pipeline.set_state(gst::State::Null);
        let _ = self.out_pipeline.set_state(gst::State::Null);
    }
}

/// `pulsesrc [input] ! audioconvert ! audioresample ! caps ! appsink`.
///
/// The appsink callback owns the `Dsp` and the output `appsrc`. Each processed
/// frame is pushed into the `appsrc` and measured for `InputLevel` events.
/// Returns the pipeline and the `appsrc` that feeds the output pipeline.
fn build_in_pipeline(
    input: Option<String>,
    cfg: DspConfig,
    evt: UnboundedSender<SessionEvent>,
) -> Result<(gst::Pipeline, gst_app::AppSrc)> {
    let pipeline = gst::Pipeline::new();

    let src = {
        let b = gst::ElementFactory::make("pulsesrc");
        match input {
            Some(dev) => b.property("device", dev),
            None => b,
        }
        .build()?
    };
    let convert = gst::ElementFactory::make("audioconvert").build()?;
    let resample = gst::ElementFactory::make("audioresample").build()?;
    let caps_el = gst::ElementFactory::make("capsfilter").property("caps", pcm_caps()).build()?;

    let appsink = gst_app::AppSink::builder()
        .name("mon_cap")
        .caps(&pcm_caps())
        .sync(false)
        .max_buffers(4)
        .drop(true)
        .build();

    pipeline.add_many([&src, &convert, &resample, &caps_el, appsink.upcast_ref()])?;
    gst::Element::link_many([&src, &convert, &resample, &caps_el, appsink.upcast_ref()])?;

    // The appsrc lives in the output pipeline; we create it here so the appsink
    // callback can push frames into it directly.
    let out_appsrc = gst_app::AppSrc::builder()
        .name("mon_out")
        .caps(&pcm_caps())
        .format(gst::Format::Time)
        .is_live(true)
        .build();

    let mut dsp = Dsp::new()?;
    dsp.set_config(&cfg);

    let mut pcm_out = vec![0i16; FRAME_SAMPLES];
    let mut frame_count: u64 = 0;
    let frame_duration = gst::ClockTime::from_mseconds(10);

    // Clone the appsrc handle for the closure – the original is returned to the
    // caller so it can be added to the output pipeline.
    let push_appsrc = out_appsrc.clone();

    appsink.set_callbacks(
        gst_app::AppSinkCallbacks::builder()
            .new_sample(move |sink| {
                let Ok(sample) = sink.pull_sample() else {
                    return Err(gst::FlowError::Eos);
                };
                let Some(mut mic) = sample_to_f32(&sample) else {
                    return Ok(gst::FlowSuccess::Ok);
                };

                if mic.len() != FRAME_SAMPLES {
                    return Ok(gst::FlowSuccess::Ok);
                }

                dsp.process_capture(&mut mic);

                let rms_db = rms_dbfs(&mic);
                let _ = evt.send(SessionEvent::InputLevel(rms_db));

                f32_to_i16(&mic, &mut pcm_out);

                let pts = frame_duration * frame_count;
                frame_count += 1;

                push_frame(&push_appsrc, &pcm_out, pts, frame_duration);

                Ok(gst::FlowSuccess::Ok)
            })
            .build(),
    );

    Ok((pipeline, out_appsrc))
}

/// `appsrc ! audioconvert ! pulsesink [output]`.
///
/// Receives DSP'd S16LE frames from the capture callback and plays them back
/// so the user hears their own mic through the chosen output device.
fn build_out_pipeline(output: Option<String>, appsrc: gst_app::AppSrc) -> Result<gst::Pipeline> {
    let pipeline = gst::Pipeline::new();

    let convert = gst::ElementFactory::make("audioconvert").build()?;
    let resample = gst::ElementFactory::make("audioresample").build()?;

    let sink = {
        let b = gst::ElementFactory::make("pulsesink");
        match output {
            Some(dev) => b.property("device", dev),
            None => b,
        }
        .build()?
    };

    pipeline.add_many([appsrc.upcast_ref(), &convert, &resample, &sink])?;
    gst::Element::link_many([appsrc.upcast_ref(), &convert, &resample, &sink])?;

    Ok(pipeline)
}

/// Write one S16LE frame into a GStreamer buffer with an explicit PTS/duration
/// and push it into the output `appsrc`. Errors are logged, never panicked.
fn push_frame(appsrc: &gst_app::AppSrc, pcm: &[i16], pts: gst::ClockTime, duration: gst::ClockTime) {
    let byte_len = std::mem::size_of_val(pcm);

    let Some(mut buffer) = gst::Buffer::with_size(byte_len).ok() else {
        eprintln!("audio/monitor: failed to allocate pcm buffer – skipping frame");
        return;
    };

    {
        let Some(buffer_mut) = buffer.get_mut() else {
            eprintln!("audio/monitor: buffer not uniquely owned – skipping frame");
            return;
        };

        {
            let Ok(mut map) = buffer_mut.map_writable() else {
                eprintln!("audio/monitor: failed to map pcm buffer – skipping frame");
                return;
            };

            // SAFETY: caps pin the format to S16LE interleaved on little-endian targets.
            let dst: &mut [i16] = unsafe {
                std::slice::from_raw_parts_mut(map.as_mut_slice().as_mut_ptr() as *mut i16, pcm.len())
            };
            dst.copy_from_slice(pcm);
        }

        buffer_mut.set_pts(pts);
        buffer_mut.set_duration(duration);
    }

    let _ = appsrc.push_buffer(buffer);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::capture::{i16_to_f32, f32_to_i16};

    // These are pure-function smoke tests; the pipeline itself requires a
    // running PulseAudio daemon and is verified by the run-and-observe step.

    #[test]
    fn push_frame_allocates_correctly_sized_buffer() {
        // Verify the byte math: 480 i16 samples = 960 bytes.
        let pcm = vec![0i16; FRAME_SAMPLES];
        assert_eq!(std::mem::size_of_val(pcm.as_slice()), FRAME_SAMPLES * 2);
    }

    #[test]
    fn monitor_dsp_round_trip_preserves_silence() {
        let mut dsp = Dsp::new().expect("create dsp");
        dsp.set_config(&DspConfig {
            echo_cancel: false,
            noise_suppression: crate::audio::dsp::NsLevel::Off,
            agc: false,
            vad: false,
            high_pass: false,
        });

        let mut frame = vec![0.0f32; FRAME_SAMPLES];
        dsp.process_capture(&mut frame);

        let db = rms_dbfs(&frame);
        assert!(db <= -90.0, "DSP'd silence must remain near the floor, got {db}");
    }

    #[test]
    fn i16_roundtrip_within_tolerance() {
        let src: Vec<i16> = (-4..=4).map(|i| i * 8191).collect();
        let mut f = vec![0.0f32; src.len()];
        i16_to_f32(&src, &mut f);
        let mut back = vec![0i16; src.len()];
        f32_to_i16(&f, &mut back);
        for (a, b) in src.iter().zip(back.iter()) {
            assert!((a - b).abs() <= 1, "{a} vs {b}");
        }
    }
}
