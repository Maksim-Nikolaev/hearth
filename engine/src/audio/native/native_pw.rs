//! Native low-latency audio I/O on Linux via pipewire-rs — the PipeWire analogue
//! of `native_wasapi.rs`. Each stream owns a thread driving a `pw::MainLoop`;
//! callers see mono f32 @ 48 kHz to match Opus and the DSP frame. A pinned small
//! quantum (`node.latency`) keeps the capture period from drifting under load,
//! which is the failure mode of the GStreamer pulsesrc path over a long session.

use crate::audio::native::{FAR_END_CAP, MAX_LANE_SAMPLES, SAMPLE_RATE};
use anyhow::{bail, Result};
use pipewire as pw;
use pw::{properties::properties, spa};
use spa::param::audio::{AudioFormat, AudioInfoRaw};
use spa::pod::{serialize::PodSerializer, Object, Pod, Value};
use std::collections::{HashMap, VecDeque};
use std::io::Cursor;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};

/// Build the F32LE format param for `stream.connect`, requesting a fixed channel
/// count (1 = mono) at 48 kHz; PipeWire inserts a converter from the device's
/// native rate/layout.
fn audio_format_param(channels: u32) -> Vec<u8> {
    let mut info = AudioInfoRaw::new();
    info.set_format(AudioFormat::F32LE);
    info.set_rate(SAMPLE_RATE);
    info.set_channels(channels);

    let obj = Object {
        type_: spa::utils::SpaTypes::ObjectParamFormat.as_raw(),
        id: spa::param::ParamType::EnumFormat.as_raw(),
        properties: info.into(),
    };

    PodSerializer::serialize(Cursor::new(Vec::new()), &Value::Object(obj))
        .unwrap()
        .0
        .into_inner()
}

/// The `node.latency` value from the env-tunable quantum (see [`quantum_prop`]).
fn latency_prop() -> String {
    quantum_prop(std::env::var("HEARTH_PW_QUANTUM").ok().as_deref())
}

// ── Pure helpers (unit-tested without a PipeWire server) ──────────────────────

/// Average all channels of an interleaved f32 buffer into mono.
pub(crate) fn downmix_to_mono(interleaved: &[f32], channels: usize, out: &mut Vec<f32>) {
    out.clear();
    if channels == 0 {
        return;
    }
    for frame in interleaved.chunks_exact(channels) {
        out.push(frame.iter().sum::<f32>() / channels as f32);
    }
}

/// Append `samples` to a playback lane, trimming oldest so the lane never exceeds
/// `max` (newest-wins; backlog is pure added latency).
pub(crate) fn enqueue_trim(q: &mut VecDeque<f32>, samples: &[f32], max: usize) {
    q.extend(samples.iter().copied());
    while q.len() > max {
        q.pop_front();
    }
}

/// Append rendered mono to the AEC far-end ring, capped at `cap` (drop oldest).
pub(crate) fn push_far(ring: &mut VecDeque<f32>, samples: &[f32], cap: usize) {
    ring.extend(samples.iter().copied());
    while ring.len() > cap {
        ring.pop_front();
    }
}

/// The `node.latency` stream property: a fixed quantum like `"256/48000"`.
/// Honors `HEARTH_PW_QUANTUM` when it parses as `<num>/<rate>`, else defaults.
pub(crate) fn quantum_prop(env: Option<&str>) -> String {
    const DEFAULT: &str = "256/48000";
    match env {
        Some(s)
            if s.split_once('/')
                .is_some_and(|(a, b)| a.parse::<u32>().is_ok() && b.parse::<u32>().is_ok()) =>
        {
            s.to_string()
        }
        _ => DEFAULT.to_string(),
    }
}

// ── Capture ───────────────────────────────────────────────────────────────────

/// The negotiated channel count, shared from `param_changed` to `process`.
struct CaptureState {
    channels: u32,
}

/// Running mic capture. Dropping it quits the loop and joins the thread.
pub struct NativeCapture {
    quit_tx: pw::channel::Sender<()>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl NativeCapture {
    /// Start capturing `device` (or the PipeWire default source if `None`/empty).
    /// `on_frame` runs on the PipeWire RT thread with mono f32 @ 48 kHz in
    /// fixed-quantum chunks.
    pub fn start<F>(device: Option<String>, on_frame: F) -> Result<Self>
    where
        F: FnMut(&[f32]) + Send + 'static,
    {
        let (quit_tx, quit_rx) = pw::channel::channel::<()>();
        let (ready_tx, ready_rx) = mpsc::channel::<Result<(), String>>();

        let handle = std::thread::Builder::new()
            .name("native-pw-capture".into())
            .spawn(move || {
                if let Err(e) = run_capture(device, on_frame, quit_rx, &ready_tx) {
                    let _ = ready_tx.send(Err(e.to_string()));
                }
            })?;

        match ready_rx.recv() {
            Ok(Ok(())) => Ok(Self { quit_tx, handle: Some(handle) }),
            Ok(Err(e)) => bail!("native pw capture init: {e}"),
            Err(_) => bail!("native pw capture thread exited before init"),
        }
    }
}

impl Drop for NativeCapture {
    fn drop(&mut self) {
        let _ = self.quit_tx.send(());
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

fn run_capture<F>(
    device: Option<String>,
    on_frame: F,
    quit_rx: pw::channel::Receiver<()>,
    ready: &mpsc::Sender<Result<(), String>>,
) -> Result<()>
where
    F: FnMut(&[f32]) + Send + 'static,
{
    pw::init();
    let mainloop = pw::main_loop::MainLoop::new(None)?;
    let context = pw::context::Context::new(&mainloop)?;
    let core = context.connect(None)?;

    // Quit the loop when `NativeCapture` drops (its `quit_tx` fires).
    let _quit = quit_rx.attach(mainloop.loop_(), {
        let ml = mainloop.clone();
        move |_| ml.quit()
    });

    let mut props = properties! {
        *pw::keys::MEDIA_TYPE => "Audio",
        *pw::keys::MEDIA_CATEGORY => "Capture",
        *pw::keys::MEDIA_ROLE => "Communication",
        *pw::keys::NODE_NAME => "hearth-voice-capture",
        *pw::keys::NODE_LATENCY => latency_prop(),
    };
    if let Some(id) = device.filter(|s| !s.is_empty()) {
        props.insert(*pw::keys::TARGET_OBJECT, id);
    }

    let stream = pw::stream::Stream::new(&core, "hearth-voice-capture", props)?;

    let mut on_frame = on_frame;
    let mut mono: Vec<f32> = Vec::with_capacity(SAMPLE_RATE as usize / 50);

    let _listener = stream
        .add_local_listener_with_user_data(CaptureState { channels: 1 })
        .param_changed(|_, state, id, param| {
            let Some(param) = param else { return };
            if id != pw::spa::param::ParamType::Format.as_raw() {
                return;
            }
            let mut info = AudioInfoRaw::new();
            if info.parse(param).is_ok() {
                state.channels = info.channels().max(1);
            }
        })
        .process(move |stream, state| {
            while let Some(mut buffer) = stream.dequeue_buffer() {
                let datas = buffer.datas_mut();
                let Some(data) = datas.first_mut() else { continue };
                let n_bytes = data.chunk().size() as usize;
                if n_bytes == 0 {
                    continue;
                }
                if let Some(slice) = data.data() {
                    // Truncate to a whole number of f32 samples before casting.
                    let n = n_bytes.min(slice.len()) & !3;
                    let samples: &[f32] = bytemuck::cast_slice(&slice[..n]);
                    downmix_to_mono(samples, state.channels as usize, &mut mono);
                    if !mono.is_empty() {
                        on_frame(&mono);
                    }
                }
            }
        })
        .register()?;

    let values = audio_format_param(1);
    let mut params = [Pod::from_bytes(&values).unwrap()];
    stream.connect(
        spa::utils::Direction::Input,
        None,
        pw::stream::StreamFlags::AUTOCONNECT
            | pw::stream::StreamFlags::MAP_BUFFERS
            | pw::stream::StreamFlags::RT_PROCESS,
        &mut params,
    )?;

    let _ = ready.send(Ok(()));
    mainloop.run();
    Ok(())
}

// ── Playback ──────────────────────────────────────────────────────────────────

/// Running speaker playback with a built-in mixer: each source `push`es mono f32
/// @ 48 kHz and the render thread sums them. Dropping it quits the loop and joins.
pub struct NativePlayback {
    quit_tx: pw::channel::Sender<()>,
    sources: Arc<Mutex<HashMap<u64, VecDeque<f32>>>>,
    far_end: Arc<Mutex<VecDeque<f32>>>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl NativePlayback {
    pub fn start(device: Option<String>) -> Result<Self> {
        let sources: Arc<Mutex<HashMap<u64, VecDeque<f32>>>> = Arc::new(Mutex::new(HashMap::new()));
        let far_end: Arc<Mutex<VecDeque<f32>>> = Arc::new(Mutex::new(VecDeque::new()));
        let (quit_tx, quit_rx) = pw::channel::channel::<()>();
        let (ready_tx, ready_rx) = mpsc::channel::<Result<(), String>>();

        let sources_t = sources.clone();
        let far_t = far_end.clone();
        let handle = std::thread::Builder::new()
            .name("native-pw-playback".into())
            .spawn(move || {
                if let Err(e) = run_playback(device, sources_t, far_t, quit_rx, &ready_tx) {
                    let _ = ready_tx.send(Err(e.to_string()));
                }
            })?;

        match ready_rx.recv() {
            Ok(Ok(())) => Ok(Self { quit_tx, sources, far_end, handle: Some(handle) }),
            Ok(Err(e)) => bail!("native pw playback init: {e}"),
            Err(_) => bail!("native pw playback thread exited before init"),
        }
    }

    /// The rendered speaker mix (AEC far-end reference).
    pub fn far_end(&self) -> Arc<Mutex<VecDeque<f32>>> {
        self.far_end.clone()
    }

    /// Queue mono f32 @ 48 kHz for `source`'s lane, trimmed to a tight target.
    pub fn push(&self, source: u64, mono: &[f32]) {
        let mut sources = self.sources.lock().unwrap();
        enqueue_trim(sources.entry(source).or_default(), mono, MAX_LANE_SAMPLES);
    }

    /// Drop a source's lane (peer left).
    pub fn remove_source(&self, source: u64) {
        self.sources.lock().unwrap().remove(&source);
    }
}

impl Drop for NativePlayback {
    fn drop(&mut self) {
        let _ = self.quit_tx.send(());
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

fn run_playback(
    device: Option<String>,
    sources: Arc<Mutex<HashMap<u64, VecDeque<f32>>>>,
    far_end: Arc<Mutex<VecDeque<f32>>>,
    quit_rx: pw::channel::Receiver<()>,
    ready: &mpsc::Sender<Result<(), String>>,
) -> Result<()> {
    pw::init();
    let mainloop = pw::main_loop::MainLoop::new(None)?;
    let context = pw::context::Context::new(&mainloop)?;
    let core = context.connect(None)?;

    let _quit = quit_rx.attach(mainloop.loop_(), {
        let ml = mainloop.clone();
        move |_| ml.quit()
    });

    let mut props = properties! {
        *pw::keys::MEDIA_TYPE => "Audio",
        *pw::keys::MEDIA_CATEGORY => "Playback",
        *pw::keys::MEDIA_ROLE => "Communication",
        *pw::keys::NODE_NAME => "hearth-voice-playback",
        *pw::keys::NODE_LATENCY => latency_prop(),
    };
    if let Some(id) = device.filter(|s| !s.is_empty()) {
        props.insert(*pw::keys::TARGET_OBJECT, id);
    }

    let stream = pw::stream::Stream::new(&core, "hearth-voice-playback", props)?;

    // Reused across callbacks so the RT render thread never allocates.
    let mut rendered: Vec<f32> = Vec::with_capacity(2048);

    let _listener = stream
        .add_local_listener_with_user_data(())
        .process(move |stream, _| {
            let Some(mut buffer) = stream.dequeue_buffer() else { return };
            let datas = buffer.datas_mut();
            let Some(data) = datas.first_mut() else { return };

            let stride = std::mem::size_of::<f32>();
            let n_frames = match data.data() {
                Some(slice) => {
                    let n = slice.len() / stride;
                    let out: &mut [f32] = bytemuck::cast_slice_mut(&mut slice[..n * stride]);

                    rendered.clear();
                    let mut src = sources.lock().unwrap();
                    for o in out.iter_mut() {
                        // Mix: sum one sample from every source lane (silence on underrun).
                        let mut v = 0.0f32;
                        for q in src.values_mut() {
                            if let Some(s) = q.pop_front() {
                                v += s;
                            }
                        }
                        v = crate::audio::native::soft_clip(v); // gentle limiter
                        *o = v;
                        rendered.push(v);
                    }
                    drop(src);

                    // Tap the rendered mono as the AEC far-end reference.
                    push_far(&mut far_end.lock().unwrap(), &rendered, FAR_END_CAP);
                    n
                }
                None => 0,
            };

            let chunk = data.chunk_mut();
            *chunk.offset_mut() = 0;
            *chunk.stride_mut() = stride as _;
            *chunk.size_mut() = (stride * n_frames) as _;
        })
        .register()?;

    let values = audio_format_param(1);
    let mut params = [Pod::from_bytes(&values).unwrap()];
    stream.connect(
        spa::utils::Direction::Output,
        None,
        pw::stream::StreamFlags::AUTOCONNECT
            | pw::stream::StreamFlags::MAP_BUFFERS
            | pw::stream::StreamFlags::RT_PROCESS,
        &mut params,
    )?;

    let _ = ready.send(Ok(()));
    mainloop.run();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn downmix_averages_channels() {
        let interleaved = [0.0, 1.0, 0.5, -0.5]; // 2 frames, 2 ch
        let mut out = Vec::new();
        downmix_to_mono(&interleaved, 2, &mut out);
        assert_eq!(out, vec![0.5, 0.0]);
    }

    #[test]
    fn enqueue_trim_caps_to_newest() {
        let mut q: VecDeque<f32> = VecDeque::new();
        enqueue_trim(&mut q, &[1.0, 2.0, 3.0], 2);
        assert_eq!(q.iter().copied().collect::<Vec<_>>(), vec![2.0, 3.0]);
    }

    #[test]
    fn push_far_drops_oldest_past_cap() {
        let mut ring: VecDeque<f32> = VecDeque::from(vec![0.0, 0.0]);
        push_far(&mut ring, &[1.0, 2.0, 3.0], 3);
        assert_eq!(ring.len(), 3);
        assert_eq!(ring.back().copied(), Some(3.0));
    }

    #[test]
    fn quantum_prop_defaults_and_overrides() {
        assert_eq!(quantum_prop(None), "256/48000");
        assert_eq!(quantum_prop(Some("480/48000")), "480/48000");
        assert_eq!(quantum_prop(Some("garbage")), "256/48000");
    }
}
