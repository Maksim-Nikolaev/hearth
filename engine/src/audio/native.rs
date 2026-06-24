//! Native low-latency audio I/O (Phase 2) — WASAPI `IAudioClient3` capture and
//! playback as reusable, thread-driven streams, to replace GStreamer `wasapi2`
//! on the voice path.
//!
//! The spike (`wasapi3.rs`) proved a tight native passthrough feels near the OS
//! "Listen to this device" floor and runs ~60 ms under the GStreamer voice app —
//! that gap is GStreamer element overhead + loose buffering, not the device
//! periods (driver-capped at 10 ms here). This module turns that into:
//!
//! - [`NativeCapture`] — opens the default mic, delivers **mono f32 @ 48 kHz** in
//!   ~device-period chunks to a callback on its own thread.
//! - [`NativePlayback`] — opens the default speaker; callers `push` **mono f32 @
//!   48 kHz** and a thread renders it with a tight (~2-period) render-ahead.
//!
//! Channel/up-mix conversion is internal; callers always see mono 48 kHz to match
//! the Opus codec and the DSP frame. Each stream owns a COM-initialized thread.

use anyhow::{bail, Result};
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use windows::core::HSTRING;
use windows::Win32::Foundation::{HANDLE, WAIT_OBJECT_0};
use windows::Win32::Media::Audio::{
    eCapture, eConsole, eRender, IAudioCaptureClient, IAudioClient3, IAudioRenderClient,
    IMMDeviceEnumerator, MMDeviceEnumerator, AUDCLNT_BUFFERFLAGS_SILENT, AUDCLNT_SHAREMODE_SHARED,
    AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM, AUDCLNT_STREAMFLAGS_EVENTCALLBACK,
    AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY, WAVEFORMATEX,
};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_ALL, COINIT_MULTITHREADED,
};
use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObject};

/// Working sample rate the rest of the engine (Opus, DSP) expects.
pub const SAMPLE_RATE: u32 = 48000;

/// Max per-source playback backlog (~20 ms). Caps the mixer-lane latency; the
/// shared engine clock means no drift to buffer against, so keep it shallow.
const MAX_LANE_SAMPLES: usize = (SAMPLE_RATE as usize) * 20 / 1000;

/// A device opened at its minimum shared engine period, event-driven.
struct DeviceStream {
    client: IAudioClient3,
    event: HANDLE,
    channels: usize,
    period_frames: u32,
}

/// Open a capture/render endpoint at the minimum shared engine period. `device`
/// is a device id to honor (from the Settings picker); if it can't be resolved
/// (id format differs from WASAPI's), fall back to the default endpoint. Caller
/// must have initialized COM on this thread.
unsafe fn open_device(capture: bool, device: Option<&str>) -> Result<DeviceStream> {
    let enumerator: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
    let dataflow = if capture { eCapture } else { eRender };
    eprintln!("[native] open {} requested device={:?}", if capture { "capture" } else { "render" }, device);
    let dev = match device.filter(|s| !s.is_empty()) {
        Some(id) => match enumerator.GetDevice(&HSTRING::from(id)) {
            Ok(d) => d,
            Err(e) => {
                eprintln!("[native] device {id:?} not resolvable ({e}); using default endpoint");
                enumerator.GetDefaultAudioEndpoint(dataflow, eConsole)?
            }
        },
        None => enumerator.GetDefaultAudioEndpoint(dataflow, eConsole)?,
    };
    let client: IAudioClient3 = dev.Activate(CLSCTX_ALL, None)?;
    let fmt = client.GetMixFormat()?;
    let rate = (*fmt).nSamplesPerSec;
    let channels = (*fmt).nChannels as usize;

    if rate == SAMPLE_RATE {
        // Fast path: the device engine already runs at 48 kHz — use the minimum
        // shared engine period (IAudioClient3) for the lowest latency.
        let (mut def, mut fund, mut min_p, mut max_p) = (0u32, 0u32, 0u32, 0u32);
        client.GetSharedModeEnginePeriod(fmt, &mut def, &mut fund, &mut min_p, &mut max_p)?;
        client.InitializeSharedAudioStream(AUDCLNT_STREAMFLAGS_EVENTCALLBACK, min_p, fmt, None)?;
        let event = CreateEventW(None, false, false, None)?;
        client.SetEventHandle(event)?;
        return Ok(DeviceStream { client, event, channels, period_frames: min_p });
    }

    // The device runs at another rate (e.g. 44.1 kHz). Keep our 48 kHz pipeline
    // and let the WASAPI shared engine resample, by initializing with an explicit
    // 48 kHz float format + AUTOCONVERTPCM instead of the engine-period stream.
    eprintln!("[native] device is {rate} Hz; using AUTOCONVERTPCM resampling to 48 kHz");
    let nch = channels as u16;
    let wfx = WAVEFORMATEX {
        wFormatTag: 3, // WAVE_FORMAT_IEEE_FLOAT
        nChannels: nch,
        nSamplesPerSec: SAMPLE_RATE,
        nAvgBytesPerSec: SAMPLE_RATE * nch as u32 * 4,
        nBlockAlign: nch * 4,
        wBitsPerSample: 32,
        cbSize: 0,
    };
    let buf_dur: i64 = 200_000; // 20 ms (100-ns units)
    client.Initialize(
        AUDCLNT_SHAREMODE_SHARED,
        AUDCLNT_STREAMFLAGS_EVENTCALLBACK
            | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM
            | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY,
        buf_dur,
        0,
        &wfx,
        None,
    )?;
    let event = CreateEventW(None, false, false, None)?;
    client.SetEventHandle(event)?;
    let period_frames = client.GetBufferSize()?;
    Ok(DeviceStream { client, event, channels, period_frames })
}

// ── Capture ─────────────────────────────────────────────────────────────────

/// Running mic capture. Dropping it stops and joins the capture thread.
pub struct NativeCapture {
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl NativeCapture {
    /// Start capturing `device` (or the default mic if `None`). `on_frame` runs
    /// on the capture thread with mono f32 @ 48 kHz (downmixed) in ~device-period
    /// chunks (~10 ms).
    pub fn start<F>(device: Option<String>, on_frame: F) -> Result<Self>
    where
        F: FnMut(&[f32]) + Send + 'static,
    {
        let stop = Arc::new(AtomicBool::new(false));
        let stop_thread = stop.clone();
        let (ready_tx, ready_rx) = mpsc::channel::<Result<(), String>>();

        let handle = std::thread::Builder::new()
            .name("native-capture".into())
            .spawn(move || {
                let r = unsafe { capture_loop(&stop_thread, device.as_deref(), on_frame, &ready_tx) };
                if let Err(e) = r {
                    let _ = ready_tx.send(Err(e.to_string()));
                }
            })?;

        match ready_rx.recv() {
            Ok(Ok(())) => Ok(Self { stop, handle: Some(handle) }),
            Ok(Err(e)) => bail!("native capture init: {e}"),
            Err(_) => bail!("native capture thread exited before init"),
        }
    }
}

impl Drop for NativeCapture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

unsafe fn capture_loop<F>(
    stop: &AtomicBool,
    device: Option<&str>,
    mut on_frame: F,
    ready: &mpsc::Sender<Result<(), String>>,
) -> Result<()>
where
    F: FnMut(&[f32]),
{
    let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    let dev = open_device(true, device)?;
    let svc: IAudioCaptureClient = dev.client.GetService()?;
    let ch = dev.channels;
    dev.client.Start()?;
    let _ = ready.send(Ok(()));

    let mut mono: Vec<f32> = Vec::with_capacity(dev.period_frames as usize);
    while !stop.load(Ordering::Relaxed) {
        if WaitForSingleObject(dev.event, 100) != WAIT_OBJECT_0 {
            continue;
        }
        loop {
            if svc.GetNextPacketSize()? == 0 {
                break;
            }
            let mut data: *mut u8 = std::ptr::null_mut();
            let (mut frames, mut flags) = (0u32, 0u32);
            svc.GetBuffer(&mut data, &mut frames, &mut flags, None, None)?;
            let silent = (flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32) != 0;
            mono.clear();
            let samples = std::slice::from_raw_parts(data as *const f32, frames as usize * ch);
            for f in 0..frames as usize {
                // downmix to mono by averaging channels
                let mut acc = 0.0f32;
                if !silent {
                    for c in 0..ch {
                        acc += samples[f * ch + c];
                    }
                    acc /= ch as f32;
                }
                mono.push(acc);
            }
            svc.ReleaseBuffer(frames)?;
            if !mono.is_empty() {
                on_frame(&mono);
            }
        }
    }

    let _ = dev.client.Stop();
    CoUninitialize();
    Ok(())
}

// ── Playback ────────────────────────────────────────────────────────────────

/// Running speaker playback with a built-in mixer: each source (e.g. one per
/// remote peer) `push`es mono f32 @ 48 kHz and the render thread sums them.
/// Dropping it stops and joins the thread.
pub struct NativePlayback {
    stop: Arc<AtomicBool>,
    sources: Arc<Mutex<HashMap<u64, VecDeque<f32>>>>,
    /// Rendered mono mix (the speaker signal) — the AEC far-end reference.
    far_end: Arc<Mutex<VecDeque<f32>>>,
    handle: Option<std::thread::JoinHandle<()>>,
}

/// Cap the far-end ring at ~200 ms so it can't grow unbounded when no AEC is
/// consuming it (drop oldest).
const FAR_END_CAP: usize = SAMPLE_RATE as usize / 5;

impl NativePlayback {
    pub fn start(device: Option<String>) -> Result<Self> {
        let stop = Arc::new(AtomicBool::new(false));
        let sources: Arc<Mutex<HashMap<u64, VecDeque<f32>>>> = Arc::new(Mutex::new(HashMap::new()));
        let far_end: Arc<Mutex<VecDeque<f32>>> = Arc::new(Mutex::new(VecDeque::new()));
        let stop_thread = stop.clone();
        let sources_thread = sources.clone();
        let far_thread = far_end.clone();
        let (ready_tx, ready_rx) = mpsc::channel::<Result<(), String>>();

        let handle = std::thread::Builder::new()
            .name("native-playback".into())
            .spawn(move || {
                let r = unsafe {
                    playback_loop(&stop_thread, device.as_deref(), &sources_thread, &far_thread, &ready_tx)
                };
                if let Err(e) = r {
                    let _ = ready_tx.send(Err(e.to_string()));
                }
            })?;

        match ready_rx.recv() {
            Ok(Ok(())) => Ok(Self { stop, sources, far_end, handle: Some(handle) }),
            Ok(Err(e)) => bail!("native playback init: {e}"),
            Err(_) => bail!("native playback thread exited before init"),
        }
    }

    /// The rendered speaker mix (AEC far-end reference). The capture thread pulls
    /// from this in lock-step with the mic to cancel echo.
    pub fn far_end(&self) -> Arc<Mutex<VecDeque<f32>>> {
        self.far_end.clone()
    }

    /// Queue mono f32 @ 48 kHz for `source`'s lane, trimmed to a tight target so
    /// a startup burst (UDP packets buffered before playback drains them) can't
    /// become permanent latency. WASAPI shared-mode capture+render share the
    /// engine clock, so there's no drift to absorb — keep the lane shallow.
    pub fn push(&self, source: u64, mono: &[f32]) {
        let mut sources = self.sources.lock().unwrap();
        let q = sources.entry(source).or_default();
        q.extend(mono.iter().copied());
        while q.len() > MAX_LANE_SAMPLES {
            q.pop_front();
        }
    }

    /// Drop a source's lane (peer left).
    pub fn remove_source(&self, source: u64) {
        self.sources.lock().unwrap().remove(&source);
    }
}

// ── Mic test monitor ─────────────────────────────────────────────────────────

/// Mic → your own speakers loopback for the Settings mic test: captures the mic,
/// emits the input level, and plays it back **only when the activation gate is
/// open** (so PTT / voice-activity behave exactly like a real call). Drop to stop.
pub struct NativeMonitor {
    _capture: NativeCapture,
    _playback: std::sync::Arc<NativePlayback>,
}

impl NativeMonitor {
    pub fn start(
        gate: Arc<std::sync::Mutex<crate::audio::gate::Gate>>,
        evt_tx: tokio::sync::mpsc::UnboundedSender<crate::session::SessionEvent>,
        input_device: Option<String>,
        output_device: Option<String>,
    ) -> Result<Self> {
        let playback = std::sync::Arc::new(NativePlayback::start(output_device)?);
        let pb = playback.clone();
        let mut mon_gain = 0.0f32;
        let capture = NativeCapture::start(input_device, move |mono| {
            let rms = rms_dbfs(mono);
            let _ = evt_tx.send(crate::session::SessionEvent::InputLevel(rms));
            let open = {
                let mut g = gate.lock().unwrap();
                g.update_level(rms, false); // no VAD assist — gate purely by threshold
                g.monitor_open() // threshold + hold — ignore mute/suspend
            };
            // Ramp in/out so crossing the threshold is click-free.
            let mut out = mono.to_vec();
            let target = if open { 1.0 } else { super::native_voice::FLOOR_GAIN };
            super::native_voice::ramp_gain(&mut out, &mut mon_gain, target);
            pb.push(0, &out);
        })?;
        Ok(Self { _capture: capture, _playback: playback })
    }
}

/// Gentle limiter for the summed mix: identity up to ±0.95, then a smooth tanh
/// knee asymptoting to ±1.0. Avoids the harsh square-wave distortion of a
/// brick-wall clamp (which also drives the acoustic echo loop harder).
pub(crate) fn soft_clip(x: f32) -> f32 {
    const T: f32 = 0.95;
    let a = x.abs();
    if a <= T {
        x
    } else {
        x.signum() * (T + (1.0 - T) * ((a - T) / (1.0 - T)).tanh())
    }
}

fn rms_dbfs(frame: &[f32]) -> f32 {
    if frame.is_empty() {
        return -120.0;
    }
    let sum: f32 = frame.iter().map(|s| s * s).sum();
    let rms = (sum / frame.len() as f32).sqrt();
    if rms <= 1e-7 {
        -120.0
    } else {
        20.0 * rms.log10()
    }
}

impl Drop for NativePlayback {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

unsafe fn playback_loop(
    stop: &AtomicBool,
    device: Option<&str>,
    sources: &Mutex<HashMap<u64, VecDeque<f32>>>,
    far_end: &Mutex<VecDeque<f32>>,
    ready: &mpsc::Sender<Result<(), String>>,
) -> Result<()> {
    let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    let dev = open_device(false, device)?;
    let svc: IAudioRenderClient = dev.client.GetService()?;
    let ch = dev.channels;
    let buf_frames = dev.client.GetBufferSize()?;
    dev.client.Start()?;
    let _ = ready.send(Ok(()));

    let mut cycle: u64 = 0;
    while !stop.load(Ordering::Relaxed) {
        if WaitForSingleObject(dev.event, 100) != WAIT_OBJECT_0 {
            continue;
        }
        // Periodically report the deepest mixer lane (its backlog == added latency).
        cycle += 1;
        if cycle % 200 == 0 {
            let max = sources.lock().unwrap().values().map(|q| q.len()).max().unwrap_or(0);
            if max > 0 {
                eprintln!("[native] playback lane backlog: {:.1} ms", max as f64 / SAMPLE_RATE as f64 * 1000.0);
            }
        }
        // Keep ~1 period queued (not 2): the stream buffer is ~2 periods, so one
        // period of slack remains for scheduling — tighter render latency.
        let target_ahead = dev.period_frames.min(buf_frames);
        let padding = dev.client.GetCurrentPadding()?;
        if padding >= target_ahead {
            continue;
        }
        let to_write = target_ahead - padding;
        let data = svc.GetBuffer(to_write)?;
        let out = std::slice::from_raw_parts_mut(data as *mut f32, to_write as usize * ch);
        {
            let mut sources = sources.lock().unwrap();
            let mut far = far_end.lock().unwrap();
            for f in 0..to_write as usize {
                // mix: sum one sample from every source lane (silence on underrun)
                let mut v = 0.0f32;
                for q in sources.values_mut() {
                    if let Some(s) = q.pop_front() {
                        v += s;
                    }
                }
                v = soft_clip(v); // gentle limiter, not a brick-wall clip
                for c in 0..ch {
                    out[f * ch + c] = v; // mono -> all channels
                }
                // Tap the rendered mono as the AEC far-end reference.
                far.push_back(v);
            }
            while far.len() > FAR_END_CAP {
                far.pop_front();
            }
        }
        svc.ReleaseBuffer(to_write, 0)?;
    }

    let _ = dev.client.Stop();
    CoUninitialize();
    Ok(())
}

#[cfg(test)]
mod tests {
    use audiopus::{coder::Decoder, coder::Encoder, Application, Channels, SampleRate};

    // De-risk: confirm the bundled libopus links and round-trips at runtime
    // (10 ms mono @ 48 kHz, low-delay) — the codec the native voice path uses.
    #[test]
    fn opus_lowdelay_roundtrip() {
        let enc = Encoder::new(SampleRate::Hz48000, Channels::Mono, Application::LowDelay).unwrap();
        let mut dec = Decoder::new(SampleRate::Hz48000, Channels::Mono).unwrap();

        let pcm: Vec<f32> = (0..480).map(|i| (i as f32 * 0.05).sin() * 0.25).collect();
        let mut packet = vec![0u8; 4000];
        let n = enc.encode_float(&pcm, &mut packet).unwrap();
        assert!(n > 0 && n < 4000);

        let mut out = vec![0.0f32; 480];
        let frames = dec.decode_float(Some(&packet[..n]), &mut out, false).unwrap();
        assert_eq!(frames, 480);
    }
}
