//! Native low-latency audio I/O on Linux via pipewire-rs — the PipeWire analogue
//! of `native_wasapi.rs`. Each stream owns a thread driving a `pw::MainLoop`;
//! callers see mono f32 @ 48 kHz to match Opus and the DSP frame. A pinned small
//! quantum (`node.latency`) keeps the capture period from drifting under load,
//! which is the failure mode of the GStreamer pulsesrc path over a long session.

use crate::audio::native::MAX_LANE_SAMPLES;
use anyhow::{bail, Result};
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};

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

/// Running mic capture. Dropping it stops and joins the capture thread.
#[allow(dead_code)] // fields wired up in the stream implementation (Task 4)
pub struct NativeCapture {
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl NativeCapture {
    /// Start capturing `device` (or the default mic if `None`). `on_frame` runs
    /// on the PipeWire RT thread with mono f32 @ 48 kHz in fixed-quantum chunks.
    pub fn start<F>(_device: Option<String>, _on_frame: F) -> Result<Self>
    where
        F: FnMut(&[f32]) + Send + 'static,
    {
        bail!("native_pw capture not yet implemented")
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

// ── Playback ──────────────────────────────────────────────────────────────────

/// Running speaker playback with a built-in mixer: each source `push`es mono f32
/// @ 48 kHz and the render thread sums them. Dropping it stops and joins.
#[allow(dead_code)] // fields wired up in the stream implementation (Task 5)
pub struct NativePlayback {
    stop: Arc<AtomicBool>,
    sources: Arc<Mutex<HashMap<u64, VecDeque<f32>>>>,
    far_end: Arc<Mutex<VecDeque<f32>>>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl NativePlayback {
    pub fn start(_device: Option<String>) -> Result<Self> {
        bail!("native_pw playback not yet implemented")
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
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
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
