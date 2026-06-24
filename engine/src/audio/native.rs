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
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use windows::Win32::Foundation::{HANDLE, WAIT_OBJECT_0};
use windows::Win32::Media::Audio::{
    eCapture, eConsole, eRender, IAudioCaptureClient, IAudioClient3, IAudioRenderClient,
    IMMDeviceEnumerator, MMDeviceEnumerator, AUDCLNT_BUFFERFLAGS_SILENT,
    AUDCLNT_STREAMFLAGS_EVENTCALLBACK,
};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_ALL, COINIT_MULTITHREADED,
};
use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObject};

/// Working sample rate the rest of the engine (Opus, DSP) expects.
pub const SAMPLE_RATE: u32 = 48000;

/// A device opened at its minimum shared engine period, event-driven.
struct DeviceStream {
    client: IAudioClient3,
    event: HANDLE,
    channels: usize,
    period_frames: u32,
}

/// Open the default capture/render endpoint at the minimum shared engine period.
/// Caller must have initialized COM on this thread.
unsafe fn open_device(capture: bool) -> Result<DeviceStream> {
    let enumerator: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
    let dataflow = if capture { eCapture } else { eRender };
    let dev = enumerator.GetDefaultAudioEndpoint(dataflow, eConsole)?;
    let client: IAudioClient3 = dev.Activate(CLSCTX_ALL, None)?;
    let fmt = client.GetMixFormat()?;

    let (mut def, mut fund, mut min_p, mut max_p) = (0u32, 0u32, 0u32, 0u32);
    client.GetSharedModeEnginePeriod(fmt, &mut def, &mut fund, &mut min_p, &mut max_p)?;

    let rate = (*fmt).nSamplesPerSec;
    let channels = (*fmt).nChannels as usize;
    if rate != SAMPLE_RATE {
        // Mix format is almost always 48 kHz; resampling is out of scope here.
        bail!("native audio: device is {rate} Hz, expected {SAMPLE_RATE} Hz");
    }

    client.InitializeSharedAudioStream(AUDCLNT_STREAMFLAGS_EVENTCALLBACK, min_p, fmt, None)?;
    let event = CreateEventW(None, false, false, None)?;
    client.SetEventHandle(event)?;

    Ok(DeviceStream { client, event, channels, period_frames: min_p })
}

// ── Capture ─────────────────────────────────────────────────────────────────

/// Running mic capture. Dropping it stops and joins the capture thread.
pub struct NativeCapture {
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl NativeCapture {
    /// Start capturing the default mic. `on_frame` runs on the capture thread
    /// with mono f32 @ 48 kHz (downmixed) in ~device-period chunks (~10 ms).
    pub fn start<F>(on_frame: F) -> Result<Self>
    where
        F: FnMut(&[f32]) + Send + 'static,
    {
        let stop = Arc::new(AtomicBool::new(false));
        let stop_thread = stop.clone();
        let (ready_tx, ready_rx) = mpsc::channel::<Result<(), String>>();

        let handle = std::thread::Builder::new()
            .name("native-capture".into())
            .spawn(move || {
                let r = unsafe { capture_loop(&stop_thread, on_frame, &ready_tx) };
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
    mut on_frame: F,
    ready: &mpsc::Sender<Result<(), String>>,
) -> Result<()>
where
    F: FnMut(&[f32]),
{
    let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    let dev = open_device(true)?;
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

/// Running speaker playback. `push` queues mono f32 @ 48 kHz; the thread renders
/// it with a tight render-ahead. Dropping it stops and joins the thread.
pub struct NativePlayback {
    stop: Arc<AtomicBool>,
    ring: Arc<Mutex<VecDeque<f32>>>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl NativePlayback {
    pub fn start() -> Result<Self> {
        let stop = Arc::new(AtomicBool::new(false));
        let ring: Arc<Mutex<VecDeque<f32>>> = Arc::new(Mutex::new(VecDeque::with_capacity(SAMPLE_RATE as usize)));
        let stop_thread = stop.clone();
        let ring_thread = ring.clone();
        let (ready_tx, ready_rx) = mpsc::channel::<Result<(), String>>();

        let handle = std::thread::Builder::new()
            .name("native-playback".into())
            .spawn(move || {
                let r = unsafe { playback_loop(&stop_thread, &ring_thread, &ready_tx) };
                if let Err(e) = r {
                    let _ = ready_tx.send(Err(e.to_string()));
                }
            })?;

        match ready_rx.recv() {
            Ok(Ok(())) => Ok(Self { stop, ring, handle: Some(handle) }),
            Ok(Err(e)) => bail!("native playback init: {e}"),
            Err(_) => bail!("native playback thread exited before init"),
        }
    }

    /// Queue mono f32 @ 48 kHz for playback. Drops the oldest audio if the queue
    /// runs away (should not happen in steady state — keeps latency bounded).
    pub fn push(&self, mono: &[f32]) {
        let mut ring = self.ring.lock().unwrap();
        ring.extend(mono.iter().copied());
        let cap = (SAMPLE_RATE / 4) as usize; // 250 ms safety cap
        while ring.len() > cap {
            ring.pop_front();
        }
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
    ring: &Mutex<VecDeque<f32>>,
    ready: &mpsc::Sender<Result<(), String>>,
) -> Result<()> {
    let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    let dev = open_device(false)?;
    let svc: IAudioRenderClient = dev.client.GetService()?;
    let ch = dev.channels;
    let buf_frames = dev.client.GetBufferSize()?;
    let target_ahead = (2 * dev.period_frames).min(buf_frames);
    dev.client.Start()?;
    let _ = ready.send(Ok(()));

    while !stop.load(Ordering::Relaxed) {
        if WaitForSingleObject(dev.event, 100) != WAIT_OBJECT_0 {
            continue;
        }
        let padding = dev.client.GetCurrentPadding()?;
        if padding >= target_ahead {
            continue;
        }
        let to_write = target_ahead - padding;
        let data = svc.GetBuffer(to_write)?;
        let out = std::slice::from_raw_parts_mut(data as *mut f32, to_write as usize * ch);
        {
            let mut ring = ring.lock().unwrap();
            for f in 0..to_write as usize {
                let v = ring.pop_front().unwrap_or(0.0); // silence on underrun
                for c in 0..ch {
                    out[f * ch + c] = v; // mono -> all channels
                }
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
        let mut enc = Encoder::new(SampleRate::Hz48000, Channels::Mono, Application::LowDelay).unwrap();
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
