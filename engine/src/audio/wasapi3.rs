//! Phase 2 spike — WASAPI `IAudioClient3` low-latency shared-mode loopback.
//!
//! Goal: measure the audio *device floor* we can reach **without** an exclusive
//! lock. The GStreamer `wasapi2` path bottoms out near the legacy `IAudioClient`
//! ~10 ms shared period. `IAudioClient3` (Win10+) exposes
//! `GetSharedModeEnginePeriod` / `InitializeSharedAudioStream` to run the engine
//! at its *minimum* period (~2.67 ms @ 48 kHz) while still sharing the device.
//!
//! This opens the default mic + speaker at the minimum period and passes mic →
//! speaker, so the round trip is measurable with the same OBS method (Mic track
//! vs Entire System track). It also prints the achieved periods.
//!
//! Run: `engine.exe wasapi3 [seconds]` (default 20). Standalone — does not touch
//! the live voice path. This is a measurement spike, not the integration.

use anyhow::{bail, Result};
use std::collections::VecDeque;
use windows::Win32::Foundation::{HANDLE, WAIT_OBJECT_0};
use windows::Win32::Media::Audio::{
    eCapture, eConsole, eRender, IAudioCaptureClient, IAudioClient3, IAudioRenderClient,
    IMMDeviceEnumerator, MMDeviceEnumerator, AUDCLNT_BUFFERFLAGS_SILENT,
    AUDCLNT_STREAMFLAGS_EVENTCALLBACK,
};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CLSCTX_ALL, COINIT_MULTITHREADED,
};
use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObject};

/// One `IAudioClient3` stream opened at the minimum shared engine period.
struct Stream {
    client: IAudioClient3,
    event: HANDLE,
    rate: u32,
    channels: usize,
    period_frames: u32,
}

/// Open the default endpoint for `dataflow` at its minimum shared engine period,
/// event-driven. Prints the achieved period vs the legacy ~10 ms floor.
unsafe fn open_stream(enumerator: &IMMDeviceEnumerator, capture: bool) -> Result<Stream> {
    let dataflow = if capture { eCapture } else { eRender };
    let dev = enumerator.GetDefaultAudioEndpoint(dataflow, eConsole)?;
    let client: IAudioClient3 = dev.Activate(CLSCTX_ALL, None)?;
    let fmt = client.GetMixFormat()?;

    let (mut default_p, mut fundamental_p, mut min_p, mut max_p) = (0u32, 0u32, 0u32, 0u32);
    client.GetSharedModeEnginePeriod(fmt, &mut default_p, &mut fundamental_p, &mut min_p, &mut max_p)?;

    let rate = (*fmt).nSamplesPerSec;
    let channels = (*fmt).nChannels as usize;
    let label = if capture { "capture" } else { "render " };
    println!(
        "[wasapi3] {label} @ {rate} Hz {channels}ch  period(frames) default={default_p} min={min_p} max={max_p}  -> min {:.2} ms  (legacy IAudioClient floor ~10 ms)",
        min_p as f64 / rate as f64 * 1000.0,
    );

    client.InitializeSharedAudioStream(AUDCLNT_STREAMFLAGS_EVENTCALLBACK, min_p, fmt, None)?;
    let event = CreateEventW(None, false, false, None)?;
    client.SetEventHandle(event)?;

    // GetMixFormat allocates via CoTaskMemAlloc; we intentionally leak it (the
    // spike runs briefly), keeping the format alive through Initialize.
    Ok(Stream { client, event, rate, channels, period_frames: min_p })
}

/// Run the mic → speaker passthrough for `seconds`.
pub fn run_loopback(seconds: u64) -> Result<()> {
    unsafe { run_loopback_inner(seconds) }
}

unsafe fn run_loopback_inner(seconds: u64) -> Result<()> {
    // Already-initialized COM (different mode) is fine; ignore the result.
    let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    let enumerator: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;

    let capture = open_stream(&enumerator, true)?;
    let render = open_stream(&enumerator, false)?;
    if capture.rate != render.rate {
        bail!("capture {} Hz != render {} Hz (no resampler in the spike)", capture.rate, render.rate);
    }
    let (cap_ch, ren_ch) = (capture.channels, render.channels);

    let cap_svc: IAudioCaptureClient = capture.client.GetService()?;
    let ren_svc: IAudioRenderClient = render.client.GetService()?;
    let ren_buf_frames = render.client.GetBufferSize()?;

    // Ring of render-layout interleaved f32 samples bridging the two clocks.
    let mut ring: VecDeque<f32> = VecDeque::with_capacity(render.rate as usize * ren_ch);

    capture.client.Start()?;
    render.client.Start()?;
    println!(
        "[wasapi3] passthrough up for {seconds}s — talk into the mic and measure Mic vs Entire System in OBS. (render buffer {ren_buf_frames} frames = {:.2} ms)",
        ren_buf_frames as f64 / render.rate as f64 * 1000.0,
    );

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(seconds);
    while std::time::Instant::now() < deadline {
        // Driven by the render clock; 100 ms cap so the deadline is still checked
        // if events stall.
        if WaitForSingleObject(render.event, 100) != WAIT_OBJECT_0 {
            continue;
        }

        // 1) Drain captured audio into the ring (converted to render layout).
        loop {
            let packet = cap_svc.GetNextPacketSize()?;
            if packet == 0 {
                break;
            }
            let mut data: *mut u8 = std::ptr::null_mut();
            let (mut frames, mut flags) = (0u32, 0u32);
            cap_svc.GetBuffer(&mut data, &mut frames, &mut flags, None, None)?;
            let silent = (flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32) != 0;
            let samples = std::slice::from_raw_parts(data as *const f32, frames as usize * cap_ch);
            for f in 0..frames as usize {
                for ch in 0..ren_ch {
                    // dup mono→stereo, trim extra channels
                    let v = if silent { 0.0 } else { samples[f * cap_ch + ch.min(cap_ch - 1)] };
                    ring.push_back(v);
                }
            }
            cap_svc.ReleaseBuffer(frames)?;
        }

        // 2) Top the render buffer up to a tight target (~2 periods queued)
        //    rather than filling the whole 22 ms buffer — keeping it less full is
        //    what drops the render latency toward the OS floor.
        let padding = render.client.GetCurrentPadding()?;
        let target_ahead = (2 * render.period_frames).min(ren_buf_frames);
        if padding >= target_ahead {
            continue;
        }
        let to_write = target_ahead - padding;
        let data = ren_svc.GetBuffer(to_write)?;
        let out = std::slice::from_raw_parts_mut(data as *mut f32, to_write as usize * ren_ch);
        for s in out.iter_mut() {
            *s = ring.pop_front().unwrap_or(0.0);
        }
        ren_svc.ReleaseBuffer(to_write, 0)?;
    }

    render.client.Stop()?;
    capture.client.Stop()?;
    println!("[wasapi3] done.");
    Ok(())
}
