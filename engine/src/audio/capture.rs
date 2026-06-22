use crate::audio::dsp::{Dsp, DspConfig, FRAME_SAMPLES};
use crate::audio::gate::Gate;
use crate::session::SessionEvent;
use anyhow::Result;
use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc::UnboundedSender;

/// S16LE / 48 kHz / mono / interleaved – the only format the DSP frame loop
/// accepts. Both pipelines and every per-peer send appsrc share these caps.
pub(super) fn pcm_caps() -> gst::Caps {
    gst::Caps::builder("audio/x-raw")
        .field("format", "S16LE")
        .field("channels", 1i32)
        .field("rate", 48000i32)
        .field("layout", "interleaved")
        .build()
}

/// Convert interleaved i16 PCM to f32 in [-1, 1] and back, in fixed 10 ms frames.
pub fn i16_to_f32(src: &[i16], dst: &mut [f32]) {
    for (s, d) in src.iter().zip(dst.iter_mut()) {
        *d = *s as f32 / 32768.0;
    }
}

pub fn f32_to_i16(src: &[f32], dst: &mut [i16]) {
    for (s, d) in src.iter().zip(dst.iter_mut()) {
        *d = (s.clamp(-1.0, 1.0) * 32767.0) as i16;
    }
}

/// RMS level of a frame in dBFS. Silence yields a very negative floor
/// (clamped at -120 dB); a full-scale constant or sine sits near 0 dB.
pub fn rms_dbfs(frame: &[f32]) -> f32 {
    if frame.is_empty() {
        return -120.0;
    }

    let sum_sq: f32 = frame.iter().map(|s| s * s).sum();
    let rms = (sum_sq / frame.len() as f32).sqrt();

    if rms <= 1e-6 {
        return -120.0;
    }

    20.0 * rms.log10()
}

/// A single mic capture + one DSP, fanned out to every voice peer's send
/// `appsrc`. AEC references the real speaker mix via the output sink monitor.
///
/// The `cap` appsink callback owns the `Dsp` and does all processing on its own
/// streaming thread. The `ref` appsink callback only forwards render frames over
/// a channel the `cap` callback drains, so the `Dsp` never crosses threads.
pub struct VoiceCapture {
    mic_pipeline: gst::Pipeline,
    ref_pipeline: Option<gst::Pipeline>,
    peers: Arc<Mutex<Vec<gst_app::AppSrc>>>,
    pending_config: Arc<Mutex<Option<DspConfig>>>,
}

impl VoiceCapture {
    pub fn start(
        input: Option<String>,
        output: Option<String>,
        cfg: DspConfig,
        gate: Arc<Mutex<Gate>>,
        evt: UnboundedSender<SessionEvent>,
    ) -> Result<VoiceCapture> {
        gst::init()?;

        let peers: Arc<Mutex<Vec<gst_app::AppSrc>>> = Arc::new(Mutex::new(Vec::new()));
        let pending_config: Arc<Mutex<Option<DspConfig>>> = Arc::new(Mutex::new(None));

        // Render-reference: the output sink's monitor IS the played speaker mix,
        // so AEC needs no cross-pipeline far-end collection. Missing monitor =>
        // run without a reference (AEC still loads, just no far-end signal).
        let (ref_tx, ref_rx) = mpsc::channel::<Vec<f32>>();
        let ref_pipeline = match output {
            Some(sink) => build_ref_pipeline(&format!("{sink}.monitor"), ref_tx).ok(),
            None => None,
        };

        let mic_pipeline = build_mic_pipeline(
            input,
            cfg,
            gate,
            peers.clone(),
            pending_config.clone(),
            ref_rx,
            evt,
        )?;

        if let Some(rp) = ref_pipeline.as_ref() {
            rp.set_state(gst::State::Playing)?;
        }
        mic_pipeline.set_state(gst::State::Playing)?;

        Ok(VoiceCapture { mic_pipeline, ref_pipeline, peers, pending_config })
    }

    /// Register a peer's voice send `appsrc`; the next processed frame reaches it.
    pub fn add_peer(&self, appsrc: gst_app::AppSrc) {
        self.peers.lock().unwrap().push(appsrc);
    }

    /// Unregister a peer's `appsrc` (matched by element identity).
    pub fn remove_peer(&self, appsrc: &gst_app::AppSrc) {
        self.peers.lock().unwrap().retain(|a| a != appsrc);
    }

    /// Apply a new DSP config live; the `cap` callback picks it up next frame.
    pub fn set_config(&self, cfg: DspConfig) {
        *self.pending_config.lock().unwrap() = Some(cfg);
    }
}

impl Drop for VoiceCapture {
    fn drop(&mut self) {
        if let Some(rp) = self.ref_pipeline.as_ref() {
            let _ = rp.set_state(gst::State::Null);
        }
        let _ = self.mic_pipeline.set_state(gst::State::Null);
    }
}

/// `pulsesrc <output>.monitor ! convert/resample ! S16/48k/mono ! appsink`.
/// Each pulled 10 ms frame is forwarded as `Vec<f32>` to the `cap` callback.
fn build_ref_pipeline(monitor_device: &str, ref_tx: mpsc::Sender<Vec<f32>>) -> Result<gst::Pipeline> {
    let pipeline = gst::Pipeline::new();

    let src = gst::ElementFactory::make("pulsesrc")
        .property("device", monitor_device)
        .build()?;
    let convert = gst::ElementFactory::make("audioconvert").build()?;
    let resample = gst::ElementFactory::make("audioresample").build()?;
    let caps = gst::ElementFactory::make("capsfilter").property("caps", pcm_caps()).build()?;

    let appsink = gst_app::AppSink::builder()
        .name("ref")
        .caps(&pcm_caps())
        .sync(false)
        .max_buffers(4)
        .drop(true)
        .build();

    pipeline.add_many([&src, &convert, &resample, &caps, appsink.upcast_ref()])?;
    gst::Element::link_many([&src, &convert, &resample, &caps, appsink.upcast_ref()])?;

    appsink.set_callbacks(
        gst_app::AppSinkCallbacks::builder()
            .new_sample(move |sink| {
                let Ok(sample) = sink.pull_sample() else {
                    return Err(gst::FlowError::Eos);
                };
                if let Some(frame) = sample_to_f32(&sample) {
                    let _ = ref_tx.send(frame);
                }

                Ok(gst::FlowSuccess::Ok)
            })
            .build(),
    );

    Ok(pipeline)
}

/// `pulsesrc [device] ! convert/resample ! S16/48k/mono ! appsink`. The appsink
/// callback owns the `Dsp`, drains render frames, runs capture DSP + gate, then
/// pushes the processed (or silenced) frame into every registered peer appsrc.
#[allow(clippy::too_many_arguments)]
fn build_mic_pipeline(
    input: Option<String>,
    cfg: DspConfig,
    gate: Arc<Mutex<Gate>>,
    peers: Arc<Mutex<Vec<gst_app::AppSrc>>>,
    pending_config: Arc<Mutex<Option<DspConfig>>>,
    ref_rx: mpsc::Receiver<Vec<f32>>,
    evt: UnboundedSender<SessionEvent>,
) -> Result<gst::Pipeline> {
    let pipeline = gst::Pipeline::new();

    let src = gst::ElementFactory::make("pulsesrc");
    let src = match input {
        Some(dev) => src.property("device", dev),
        None => src,
    }
    .build()?;

    let convert = gst::ElementFactory::make("audioconvert").build()?;
    let resample = gst::ElementFactory::make("audioresample").build()?;
    let caps = gst::ElementFactory::make("capsfilter").property("caps", pcm_caps()).build()?;

    let appsink = gst_app::AppSink::builder()
        .name("cap")
        .caps(&pcm_caps())
        .sync(false)
        .max_buffers(4)
        .drop(true)
        .build();

    pipeline.add_many([&src, &convert, &resample, &caps, appsink.upcast_ref()])?;
    gst::Element::link_many([&src, &convert, &resample, &caps, appsink.upcast_ref()])?;

    let mut dsp = Dsp::new()?;
    dsp.set_config(&cfg);

    // The `cap` streaming thread owns these; nothing else touches them.
    let mut render_buf = vec![0.0f32; FRAME_SAMPLES];
    let mut pcm_out = vec![0i16; FRAME_SAMPLES];

    // Monotonic frame counter for deriving PTS. Advances by exactly 10 ms per
    // processed frame so Opus/RTP timing is frame-derived, not arrival-derived.
    let mut frame_count: u64 = 0;
    let frame_duration = gst::ClockTime::from_mseconds(10);

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

                if let Some(new_cfg) = pending_config.lock().unwrap().take() {
                    dsp.set_config(&new_cfg);
                }

                // Feed the far-end reference (full speaker mix) before capture.
                while let Ok(ref_frame) = ref_rx.try_recv() {
                    if ref_frame.len() == FRAME_SAMPLES {
                        render_buf.copy_from_slice(&ref_frame);
                        dsp.process_render(&mut render_buf);
                    }
                }

                let vad = dsp.process_capture(&mut mic);
                let rms_db = rms_dbfs(&mic);

                let open = {
                    let mut g = gate.lock().unwrap();
                    g.update_level(rms_db, vad);
                    g.open()
                };

                let _ = evt.send(SessionEvent::InputLevel(rms_db));

                // Closed gate => push silence so each Opus/RTP stream keeps stable
                // timing. Never skip the push.
                if open {
                    f32_to_i16(&mic, &mut pcm_out);
                } else {
                    pcm_out.iter_mut().for_each(|s| *s = 0);
                }

                let pts = frame_duration * frame_count;
                frame_count += 1;

                push_to_peers(&peers, &pcm_out, pts, frame_duration);

                Ok(gst::FlowSuccess::Ok)
            })
            .build(),
    );

    Ok(pipeline)
}

/// Pull an interleaved S16 buffer out of a sample as an f32 frame.
pub(super) fn sample_to_f32(sample: &gst::Sample) -> Option<Vec<f32>> {
    let buffer = sample.buffer()?;
    let map = buffer.map_readable().ok()?;

    let pcm: &[i16] = bytemuck_cast(map.as_slice());
    let mut frame = vec![0.0f32; pcm.len()];
    i16_to_f32(pcm, &mut frame);

    Some(frame)
}

/// Reinterpret a little-endian byte slice as `i16` samples. The capsfilter
/// guarantees S16LE, so length is always a multiple of two.
pub(super) fn bytemuck_cast(bytes: &[u8]) -> &[i16] {
    let len = bytes.len() / 2;
    // SAFETY: caps pin the format to S16LE interleaved, so the bytes are a valid
    // run of native-endian i16 on the little-endian targets this app supports.
    unsafe { std::slice::from_raw_parts(bytes.as_ptr() as *const i16, len) }
}

/// Copy one processed S16 frame into a fresh `gst::Buffer` with an explicit PTS
/// and duration, then push it into every registered peer appsrc. PTS is
/// frame-derived so Opus/RTP timing stays stable regardless of callback jitter.
fn push_to_peers(
    peers: &Arc<Mutex<Vec<gst_app::AppSrc>>>,
    pcm: &[i16],
    pts: gst::ClockTime,
    duration: gst::ClockTime,
) {
    let byte_len = std::mem::size_of_val(pcm);

    let Some(mut buffer) = gst::Buffer::with_size(byte_len).ok() else {
        eprintln!("audio/capture: failed to allocate pcm buffer – skipping frame");
        return;
    };

    {
        let Some(buffer_mut) = buffer.get_mut() else {
            eprintln!("audio/capture: buffer not uniquely owned – skipping frame");
            return;
        };

        {
            let Ok(mut map) = buffer_mut.map_writable() else {
                eprintln!("audio/capture: failed to map pcm buffer – skipping frame");
                return;
            };

            // SAFETY: same S16LE little-endian invariant as `bytemuck_cast`.
            let dst: &mut [i16] = unsafe {
                std::slice::from_raw_parts_mut(map.as_mut_slice().as_mut_ptr() as *mut i16, pcm.len())
            };
            dst.copy_from_slice(pcm);
        }

        buffer_mut.set_pts(pts);
        buffer_mut.set_duration(duration);
    }

    let peers = peers.lock().unwrap();
    for appsrc in peers.iter() {
        let _ = appsrc.push_buffer(buffer.clone());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pcm_round_trips_within_tolerance() {
        let src: Vec<i16> = vec![0, 16384, -16384, 32767, -32768];
        let mut f = vec![0.0f32; src.len()];
        i16_to_f32(&src, &mut f);
        let mut back = vec![0i16; src.len()];
        f32_to_i16(&f, &mut back);
        for (a, b) in src.iter().zip(back.iter()) {
            assert!((a - b).abs() <= 1, "{a} vs {b}");
        }
    }

    #[test]
    fn silence_is_very_negative_dbfs() {
        let frame = vec![0.0f32; FRAME_SAMPLES];
        assert!(rms_dbfs(&frame) <= -90.0, "silence must read near the floor");
    }

    #[test]
    fn full_scale_constant_is_near_zero_dbfs() {
        let frame = vec![1.0f32; FRAME_SAMPLES];
        assert!(rms_dbfs(&frame) >= -0.5, "full-scale constant sits near 0 dBFS");
    }

    #[test]
    fn full_scale_sine_is_near_minus_three_dbfs() {
        let frame: Vec<f32> = (0..FRAME_SAMPLES)
            .map(|n| (n as f32 / FRAME_SAMPLES as f32 * std::f32::consts::TAU).sin())
            .collect();

        let db = rms_dbfs(&frame);
        assert!((-4.0..=-2.0).contains(&db), "full-scale sine ~ -3 dBFS, got {db}");
    }
}
