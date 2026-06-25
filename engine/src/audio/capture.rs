use crate::audio::dsp::{Dsp, DspConfig, FRAME_SAMPLES};
use crate::audio::gate::Gate;
use crate::session::SessionEvent;
use anyhow::Result;
use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use std::sync::mpsc;
use std::sync::atomic::{AtomicU32, Ordering};
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

/// Scale every sample in place by `gain` (a linear multiplier). Used for the
/// user mic/speaker volume; `gain` is expected pre-clamped to `[0.0, 1.0]`.
pub(crate) fn apply_gain(frame: &mut [f32], gain: f32) {
    for s in frame.iter_mut() {
        *s *= gain;
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
    /// In-call self-monitor: when `Some`, the `cap` callback also pushes the
    /// post-DSP (pre-gate) mic frame here so you hear yourself during a mic test
    /// without opening a second capture. `None` = monitor off.
    self_monitor: Arc<Mutex<Option<gst_app::AppSrc>>>,
    monitor_pipeline: Mutex<Option<gst::Pipeline>>,
    /// Output device id for the self-monitor sink (the chosen speaker).
    output: Option<String>,
    /// User mic volume (pre-amp, f32 bits, 0.0–1.0), applied in the `cap` callback.
    input_volume: Arc<AtomicU32>,
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
        let self_monitor: Arc<Mutex<Option<gst_app::AppSrc>>> = Arc::new(Mutex::new(None));
        let input_volume = Arc::new(AtomicU32::new(1.0f32.to_bits()));

        // The self-monitor sink plays on the chosen speaker; keep the id since
        // `output` is consumed building the AEC reference below.
        let monitor_output = output.clone();

        // Render-reference: the output sink's monitor IS the played speaker mix,
        // so AEC needs no cross-pipeline far-end collection. Missing monitor =>
        // run without a reference (AEC still loads, just no far-end signal).
        let (ref_tx, ref_rx) = mpsc::channel::<Vec<f32>>();

        // The AEC far-end reference is a PulseAudio sink monitor. On Windows the
        // DSP is a passthrough (no AEC) and Pulse monitors don't exist, so skip
        // it — remote playback goes through autoaudiosink independently.
        #[cfg(target_os = "windows")]
        let ref_pipeline: Option<gst::Pipeline> = {
            let _ = (&output, ref_tx);
            None
        };
        #[cfg(not(target_os = "windows"))]
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
            self_monitor.clone(),
            ref_rx,
            input_volume.clone(),
            evt,
        )?;

        if let Some(rp) = ref_pipeline.as_ref() {
            rp.set_state(gst::State::Playing)?;
        }
        mic_pipeline.set_state(gst::State::Playing)?;

        Ok(VoiceCapture {
            mic_pipeline,
            ref_pipeline,
            peers,
            pending_config,
            self_monitor,
            monitor_pipeline: Mutex::new(None),
            output: monitor_output,
            input_volume,
        })
    }

    /// Set mic input volume (0.0–1.0). Live, applied as a pre-amp on capture.
    pub fn set_input_volume(&self, v: f64) {
        self.input_volume.store((v as f32).to_bits(), Ordering::Relaxed);
    }

    /// Toggle the in-call self-monitor (hear yourself through the live capture).
    /// Idempotent; building the playback pipeline lazily on first enable.
    pub fn set_self_monitor(&self, on: bool) {
        let mut src = self.self_monitor.lock().unwrap();

        if on {
            if src.is_some() {
                return;
            }
            match build_self_monitor(self.output.clone()) {
                Ok((pipeline, appsrc)) => {
                    let _ = pipeline.set_state(gst::State::Playing);
                    *self.monitor_pipeline.lock().unwrap() = Some(pipeline);
                    *src = Some(appsrc);
                }
                Err(e) => eprintln!("audio/capture: self-monitor start failed: {e}"),
            }
        } else {
            // Drop the appsrc first so the cap callback stops pushing, then tear
            // down the playback pipeline.
            *src = None;
            if let Some(p) = self.monitor_pipeline.lock().unwrap().take() {
                let _ = p.set_state(gst::State::Null);
            }
        }
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
        if let Some(mp) = self.monitor_pipeline.lock().unwrap().take() {
            let _ = mp.set_state(gst::State::Null);
        }
        let _ = self.mic_pipeline.set_state(gst::State::Null);
    }
}

/// `pulsesrc <output>.monitor ! convert/resample ! S16/48k/mono ! appsink`.
/// Each pulled 10 ms frame is forwarded as `Vec<f32>` to the `cap` callback.
/// Linux/macOS only — the AEC reference relies on a PulseAudio sink monitor.
#[cfg(not(target_os = "windows"))]
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
    self_monitor: Arc<Mutex<Option<gst_app::AppSrc>>>,
    ref_rx: mpsc::Receiver<Vec<f32>>,
    input_volume: Arc<AtomicU32>,
    evt: UnboundedSender<SessionEvent>,
) -> Result<gst::Pipeline> {
    let pipeline = gst::Pipeline::new();

    let src = gst::ElementFactory::make(crate::audio::capture_src_factory());
    let src = match input {
        Some(dev) => src.property("device", dev),
        None => src,
    }
    .build()?;

    // WASAPI defaults to a large capture ring buffer; low-latency mode (safe to
    // enable per the element docs) keeps mic delay minimal on Windows.
    #[cfg(target_os = "windows")]
    src.set_property("low-latency", true);

    let convert = gst::ElementFactory::make("audioconvert").build()?;
    let resample = gst::ElementFactory::make("audioresample").build()?;
    let caps = gst::ElementFactory::make("capsfilter").property("caps", pcm_caps()).build()?;

    let appsink = gst_app::AppSink::builder()
        .name("cap")
        .caps(&pcm_caps())
        .sync(false)
        // Pull the newest mic frame with no queue depth — any backlog here is
        // pure added mouth-to-ear latency. drop=true keeps it newest-wins.
        .max_buffers(1)
        .drop(true)
        .build();

    pipeline.add_many([&src, &convert, &resample, &caps, appsink.upcast_ref()])?;
    gst::Element::link_many([&src, &convert, &resample, &caps, appsink.upcast_ref()])?;

    let mut dsp = Dsp::new()?;
    dsp.set_config(&cfg);

    // The `cap` streaming thread owns these; nothing else touches them.
    let mut render_buf = vec![0.0f32; FRAME_SAMPLES];
    let silence = vec![0.0f32; FRAME_SAMPLES];

    // Monotonic frame counter for deriving PTS. Advances by exactly 10 ms per
    // processed frame so Opus/RTP timing is frame-derived, not arrival-derived
    // (clean timestamps keep the receiver's jitter buffer minimal).
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

                // Mic pre-amp (user input volume) before DSP.
                let in_vol = f32::from_bits(input_volume.load(Ordering::Relaxed));
                if (in_vol - 1.0).abs() > f32::EPSILON {
                    apply_gain(&mut mic, in_vol);
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

                let pts = frame_duration * frame_count;
                frame_count += 1;

                // Closed gate => push silence so each Opus/RTP stream keeps stable
                // timing. Never skip the push.
                let frame = if open { &mic[..] } else { &silence[..] };
                push_to_peers(&peers, frame, pts, frame_duration);

                // Self-monitor hears the processed mic regardless of the gate, so
                // a mic test is audible even while the call is suspended/muted.
                if let Some(src) = self_monitor.lock().unwrap().as_ref() {
                    if let Some(buf) = make_pcm_buffer(&mic, pts, frame_duration) {
                        let _ = src.push_buffer(buf);
                    }
                }

                Ok(gst::FlowSuccess::Ok)
            })
            .build(),
    );

    Ok(pipeline)
}

/// Decode a S16LE byte slice into f32 samples in `[-1, 1]`.
///
/// Works on any byte alignment – reads two bytes at a time via `from_le_bytes`,
/// which is the safe, endian-explicit equivalent of the former pointer cast.
pub(crate) fn bytes_to_f32(bytes: &[u8], dst: &mut Vec<f32>) {
    dst.clear();
    dst.extend(bytes.chunks_exact(2).map(|b| {
        i16::from_le_bytes([b[0], b[1]]) as f32 / 32768.0
    }));
}

/// Encode f32 samples in `[-1, 1]` into S16LE bytes.
///
/// Each sample is clamped, scaled, and written as two little-endian bytes via
/// `to_le_bytes`, matching the capsfilter's S16LE format without any pointer cast.
pub(crate) fn f32_to_bytes(src: &[f32], dst: &mut [u8]) {
    for (chunk, s) in dst.chunks_exact_mut(2).zip(src.iter()) {
        let bytes = ((s.clamp(-1.0, 1.0) * 32767.0) as i16).to_le_bytes();
        chunk[0] = bytes[0];
        chunk[1] = bytes[1];
    }
}

/// Pull an interleaved S16LE buffer out of a sample as an f32 frame.
pub(super) fn sample_to_f32(sample: &gst::Sample) -> Option<Vec<f32>> {
    let buffer = sample.buffer()?;
    let map = buffer.map_readable().ok()?;

    let mut frame = Vec::with_capacity(map.len() / 2);
    bytes_to_f32(map.as_slice(), &mut frame);

    Some(frame)
}

/// Encode a f32 frame as S16LE bytes into a fresh `gst::Buffer`, stamp it with
/// an explicit PTS and duration, then push it into every registered peer appsrc.
/// PTS is frame-derived so Opus/RTP timing stays stable regardless of callback
/// jitter — measurably tighter than letting the appsrc timestamp on push.
fn make_pcm_buffer(frame: &[f32], pts: gst::ClockTime, duration: gst::ClockTime) -> Option<gst::Buffer> {
    let byte_len = frame.len() * 2;
    let mut buffer = gst::Buffer::with_size(byte_len).ok()?;

    {
        let buffer_mut = buffer.get_mut()?;

        {
            let mut map = buffer_mut.map_writable().ok()?;
            f32_to_bytes(frame, map.as_mut_slice());
        }

        buffer_mut.set_pts(pts);
        buffer_mut.set_duration(duration);
    }

    Some(buffer)
}

fn push_to_peers(
    peers: &Arc<Mutex<Vec<gst_app::AppSrc>>>,
    frame: &[f32],
    pts: gst::ClockTime,
    duration: gst::ClockTime,
) {
    let Some(buffer) = make_pcm_buffer(frame, pts, duration) else {
        eprintln!("audio/capture: failed to build pcm buffer – skipping frame");
        return;
    };

    let peers = peers.lock().unwrap();
    for appsrc in peers.iter() {
        let _ = appsrc.push_buffer(buffer.clone());
    }
}

/// `appsrc ! audioconvert ! audioresample ! <platform sink>` for the in-call
/// self-monitor. `do-timestamp` + `sync=false` play frames on arrival, so the
/// frame-derived capture PTS (far into the stream) never wedges the sink.
fn build_self_monitor(output: Option<String>) -> Result<(gst::Pipeline, gst_app::AppSrc)> {
    let pipeline = gst::Pipeline::new();

    let appsrc = gst_app::AppSrc::builder()
        .name("selfmon")
        .caps(&pcm_caps())
        .is_live(true)
        .do_timestamp(true)
        .format(gst::Format::Time)
        .build();

    let convert = gst::ElementFactory::make("audioconvert").build()?;
    let resample = gst::ElementFactory::make("audioresample").build()?;

    let sink = gst::ElementFactory::make(crate::audio::playback_sink_factory());
    let sink = match output {
        Some(dev) => sink.property("device", dev),
        None => sink,
    }
    .property("sync", false)
    .build()?;

    pipeline.add_many([appsrc.upcast_ref(), &convert, &resample, &sink])?;
    gst::Element::link_many([appsrc.upcast_ref(), &convert, &resample, &sink])?;

    Ok((pipeline, appsrc))
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

    /// Verify `bytes_to_f32` / `f32_to_bytes` round-trip on a slice that starts
    /// at an odd byte offset, which would have been UB with the former pointer cast.
    #[test]
    fn bytes_to_f32_round_trip_non_aligned() {
        // Encode two known i16 samples into bytes, prepend a padding byte to
        // force an odd starting address, then decode from the offset slice.
        let samples: [i16; 4] = [0, 16384, -16384, 32767];
        let mut aligned_bytes: Vec<u8> = vec![0xAB]; // padding byte at index 0
        for s in &samples {
            aligned_bytes.extend_from_slice(&s.to_le_bytes());
        }

        // Slice starting at byte 1 – not 2-byte aligned in general.
        let unaligned = &aligned_bytes[1..];

        let mut decoded = Vec::new();
        bytes_to_f32(unaligned, &mut decoded);

        assert_eq!(decoded.len(), samples.len());

        let mut re_encoded = vec![0u8; samples.len() * 2];
        f32_to_bytes(&decoded, &mut re_encoded);

        for (i, s) in samples.iter().enumerate() {
            let got = i16::from_le_bytes([re_encoded[i * 2], re_encoded[i * 2 + 1]]);
            assert!((s - got).abs() <= 1, "sample {i}: {s} vs {got}");
        }
    }

    #[test]
    fn apply_gain_identity_half_zero() {
        let mut f = vec![1.0f32, -0.5, 0.25];
        apply_gain(&mut f, 1.0);
        assert_eq!(f, vec![1.0, -0.5, 0.25]);

        apply_gain(&mut f, 0.5);
        assert_eq!(f, vec![0.5, -0.25, 0.125]);

        apply_gain(&mut f, 0.0);
        assert_eq!(f, vec![0.0, 0.0, 0.0]);
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
