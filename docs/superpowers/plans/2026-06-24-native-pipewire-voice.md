# Native PipeWire Voice Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a native pipewire-rs voice capture/playback backend on Linux that mirrors the Windows WASAPI native path, made the default with automatic fall-through to the generic GStreamer path, and surface the active backend in the app UI.

**Architecture:** `native_voice.rs` (DSP → Opus → UDP send loop, per-peer recv→decode→mixer-lane, mic-test monitor) depends only on the public API of `NativeCapture`/`NativePlayback`. We split the WASAPI device I/O into `native_wasapi.rs` (`cfg(windows)`), add a `native_pw.rs` (`cfg(linux)`) backend with the identical API, keep shared helpers and `NativeMonitor` in the `native.rs` umbrella, and widen the `cfg(windows)` gates to `cfg(any(windows, linux))`. Session selection tries native first and falls through to GStreamer `voice_udp` on construction failure.

**Tech Stack:** Rust, pipewire-rs 0.8 (`pipewire` + `libspa`), audiopus (Opus), nnnoiseless/earshot/aec-rs (DSP), GTK4 desktop UI (relm4-style `WorkspaceInput`).

## Global Constraints

- Working audio format everywhere: **S16/F32 as noted, 48 kHz, mono**; `SAMPLE_RATE = 48000`.
- Native send framing is `[seq: u16 BE | opus payload]`; do not change it.
- Default quantum request: **`256/48000`**; env override `HEARTH_PW_QUANTUM` (e.g. `480/48000`).
- Selection policy (both platforms): default = native; on native construction failure → log Warning + use GStreamer for the session; `HEARTH_GSTREAMER_VOICE=1` forces generic from the start.
- macOS is unchanged (stays on GStreamer `voice_udp`).
- Commit messages: no `Co-Authored-By` / Claude attribution. Use en dashes (–), not em dashes.
- Do not auto-launch capture/voice for verification; manual runs are the user's. `cargo build` and `cargo test` are fine.
- New build dep: `libpipewire-0.3-dev`.

---

### Task 1: Add pipewire-rs dependency and build-env note

**Files:**
- Modify: `engine/Cargo.toml` (the `[target.'cfg(not(target_os = "windows"))'.dependencies]` block, or a new linux-specific block)
- Modify: `docs/findings/voice-latency-linux.md` (append a build-deps note) — create if absent

**Interfaces:**
- Produces: crates `pipewire` and `libspa` available under `cfg(target_os = "linux")`.

- [ ] **Step 1: Add the dependency**

In `engine/Cargo.toml`, add a Linux-only block (keep macOS off this dep):

```toml
[target.'cfg(target_os = "linux")'.dependencies]
pipewire = "0.8"
libspa = "0.8"
```

- [ ] **Step 2: Verify it resolves and builds**

Run: `cargo build -p hearth-engine`
Expected: PASS (the crates compile; `libpipewire-0.3-dev` must be installed — if the build errors with a missing `libpipewire-0.3` pkg-config, install it: `sudo apt install libpipewire-0.3-dev`).

- [ ] **Step 3: Record the build dep**

Append to `docs/findings/voice-latency-linux.md`:

```markdown
## Native PipeWire backend build deps
The native pipewire-rs voice backend (`native_pw.rs`) needs `libpipewire-0.3-dev`
at build time (pkg-config `libpipewire-0.3`). Without it the `pipewire`/`libspa`
crates fail to build.
```

- [ ] **Step 4: Commit**

```bash
git add engine/Cargo.toml docs/findings/voice-latency-linux.md
git commit -m "build(voice): add pipewire-rs deps for native Linux capture"
```

---

### Task 2: Extract shared native helpers and split WASAPI I/O out

This is a pure refactor: no behavior change. It makes the device backend swappable per platform while keeping the shared, platform-independent code in one place.

**Files:**
- Create: `engine/src/audio/native_wasapi.rs` (moved WASAPI device I/O)
- Modify: `engine/src/audio/native.rs` (becomes the umbrella + shared helpers)
- Modify: `engine/src/audio/mod.rs` (module wiring)

**Interfaces:**
- Produces (from `native.rs`, all `pub(crate)` or `pub(super)` as today): `SAMPLE_RATE: u32`, `MAX_LANE_SAMPLES: usize`, `FAR_END_CAP: usize`, `soft_clip(f32) -> f32`, `rms_dbfs(&[f32]) -> f32`, `NativeMonitor`, and a cfg-selected re-export of `NativeCapture` / `NativePlayback`.
- `NativeCapture::start(device: Option<String>, on_frame: impl FnMut(&[f32]) + Send + 'static) -> Result<NativeCapture>`
- `NativePlayback::start(device: Option<String>) -> Result<NativePlayback>`, `.push(source: u64, mono: &[f32])`, `.far_end() -> Arc<Mutex<VecDeque<f32>>>`, `.remove_source(source: u64)`

- [ ] **Step 1: Move WASAPI device I/O into `native_wasapi.rs`**

Cut from `native.rs` into a new `engine/src/audio/native_wasapi.rs` (prefix the whole file with `#![cfg(windows)]` is not valid for a non-crate-root; instead gate the `mod` in step 3): the `DeviceStream` struct, `open_device`, `NativeCapture` + `impl`/`Drop`, `NativePlayback` + `impl`/`Drop`, `capture_loop`, `playback_loop`, and the WASAPI `use windows::...` imports. Keep `SAMPLE_RATE`, `MAX_LANE_SAMPLES`, `FAR_END_CAP` defined in `native.rs` and `use crate::audio::native::{SAMPLE_RATE, MAX_LANE_SAMPLES, FAR_END_CAP};` from `native_wasapi.rs`.

- [ ] **Step 2: Keep shared helpers + `NativeMonitor` in `native.rs`**

`native.rs` retains `SAMPLE_RATE`, `MAX_LANE_SAMPLES`, `FAR_END_CAP`, `soft_clip`, `rms_dbfs`, `NativeMonitor`, and the `#[cfg(test)] opus_lowdelay_roundtrip` test. Add the backend selection at the bottom:

```rust
#[cfg(windows)]
mod native_wasapi;
#[cfg(windows)]
pub(crate) use native_wasapi::{NativeCapture, NativePlayback};

#[cfg(target_os = "linux")]
mod native_pw;
#[cfg(target_os = "linux")]
pub(crate) use native_pw::{NativeCapture, NativePlayback};
```

`NativeMonitor` already references `NativeCapture`/`NativePlayback` by the local path, so it now works against whichever backend is selected.

- [ ] **Step 3: Update `mod.rs` gating**

In `engine/src/audio/mod.rs`, widen the native modules:

```rust
// Native low-latency voice I/O: WASAPI on Windows, PipeWire on Linux.
#[cfg(any(target_os = "windows", target_os = "linux"))]
pub mod native;

#[cfg(any(target_os = "windows", target_os = "linux"))]
pub mod native_voice;
```

(`native_voice` will not compile on Linux until Task 6 flips its internal gate; for now keep its old `#[cfg(windows)]` inside the file so Linux skips it — see Task 6. To keep Linux building in this task, leave `native_voice`'s own top gate as `#[cfg(windows)]` and only widen `native`.)

- [ ] **Step 4: Verify Linux build (no Linux backend yet)**

Because `native_pw` does not exist yet, temporarily make the Linux re-export a stub so the tree builds: in `native.rs`, gate the Linux `mod native_pw;` block out with `#[cfg(all(target_os = "linux", feature = "never"))]` is overkill — instead, do Task 3 immediately after and treat Tasks 2+3 as one commit. For this step just verify the Windows-independent shared code compiles:

Run: `cargo build -p hearth-engine`
Expected: FAIL only with `unresolved module native_pw` (expected — fixed in Task 3). If any other error appears, the move was incomplete.

- [ ] **Step 5: Do not commit yet** — proceed to Task 3, commit them together (the tree is not green standalone).

---

### Task 3: PipeWire backend — pure helpers (TDD) + type scaffolding

Create `native_pw.rs` with the public types and the pure, unit-testable logic. The PipeWire stream internals are filled in Tasks 4–5; here the types exist and compile, and the math is tested.

**Files:**
- Create: `engine/src/audio/native_pw.rs`
- Test: inline `#[cfg(test)] mod tests` in `native_pw.rs`

**Interfaces:**
- Produces: `pub(crate) fn downmix_to_mono(interleaved: &[f32], channels: usize, out: &mut Vec<f32>)`
- Produces: `pub(crate) fn enqueue_trim(q: &mut VecDeque<f32>, samples: &[f32], max: usize)`
- Produces: `pub(crate) fn push_far(ring: &mut VecDeque<f32>, samples: &[f32], cap: usize)`
- Produces: `pub(crate) fn quantum_prop(env: Option<&str>) -> String` → e.g. `"256/48000"`
- Produces the type skeletons `NativeCapture` / `NativePlayback` (internals stubbed; filled later).

- [ ] **Step 1: Write failing tests for the pure helpers**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    #[test]
    fn downmix_averages_channels() {
        let interleaved = [0.0, 1.0,  0.5, -0.5]; // 2 frames, 2 ch
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
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p hearth-engine --lib native_pw`
Expected: FAIL (functions not defined).

- [ ] **Step 3: Implement the helpers and type skeletons**

At the top of `native_pw.rs`:

```rust
//! Native low-latency audio I/O on Linux via pipewire-rs — the PipeWire analogue
//! of `native_wasapi.rs`. Each stream owns a `pw::ThreadLoop`; callers see mono
//! f32 @ 48 kHz to match Opus and the DSP frame. A pinned small quantum
//! (`node.latency`) keeps the capture period from drifting under load.

use crate::audio::native::{FAR_END_CAP, MAX_LANE_SAMPLES, SAMPLE_RATE};
use anyhow::{bail, Result};
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};

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
        Some(s) if s.split_once('/').is_some_and(|(a, b)| {
            a.parse::<u32>().is_ok() && b.parse::<u32>().is_ok()
        }) => s.to_string(),
        _ => DEFAULT.to_string(),
    }
}
```

Then the type skeletons (internals stubbed to `bail!` until Tasks 4–5 — they compile and let `native_voice.rs` reference the types):

```rust
pub(crate) struct NativeCapture {
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl NativeCapture {
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

pub(crate) struct NativePlayback {
    stop: Arc<AtomicBool>,
    sources: Arc<Mutex<HashMap<u64, VecDeque<f32>>>>,
    far_end: Arc<Mutex<VecDeque<f32>>>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl NativePlayback {
    pub fn start(_device: Option<String>) -> Result<Self> {
        bail!("native_pw playback not yet implemented")
    }
    pub fn far_end(&self) -> Arc<Mutex<VecDeque<f32>>> {
        self.far_end.clone()
    }
    pub fn push(&self, source: u64, mono: &[f32]) {
        let mut s = self.sources.lock().unwrap();
        enqueue_trim(s.entry(source).or_default(), mono, MAX_LANE_SAMPLES);
    }
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
```

> Note: the `stop`/`handle`/`sources`/`far_end` fields are unused until Tasks 4–5; add `#[allow(dead_code)]` on the structs to keep the build warning-clean for this task only, and remove it in Task 5.

- [ ] **Step 4: Run tests to verify they pass and the tree builds on Linux**

Run: `cargo test -p hearth-engine --lib native_pw`
Expected: PASS (4 tests).
Run: `cargo build -p hearth-engine`
Expected: PASS.

- [ ] **Step 5: Commit (Tasks 2+3 together)**

```bash
git add engine/src/audio/native.rs engine/src/audio/native_wasapi.rs \
        engine/src/audio/native_pw.rs engine/src/audio/mod.rs
git commit -m "refactor(voice): split native device I/O per platform; add PipeWire helpers"
```

---

### Task 4: PipeWire capture stream

Fill `NativeCapture::start` with a real pipewire-rs capture stream on its own `ThreadLoop`. Device I/O can't be unit-tested without a live server, so the cycle is: implement → `cargo build` → user manual run. The code follows the official pipewire-rs `audio-capture` example.

**Files:**
- Modify: `engine/src/audio/native_pw.rs`

**Interfaces:**
- Consumes: `downmix_to_mono`, `quantum_prop`, `SAMPLE_RATE` from this module.
- Produces: a working `NativeCapture::start(device, on_frame)` delivering mono f32 @ 48 kHz to `on_frame` on the PipeWire RT thread.

- [ ] **Step 1: Implement the capture stream**

Replace the stubbed `NativeCapture::start` with the real implementation:

```rust
use pipewire::{self as pw, properties::properties, spa};
use spa::param::audio::{AudioInfoRaw, AudioFormat};
use spa::pod::{serialize::PodSerializer, Object, Pod, Value};
use std::io::Cursor;

impl NativeCapture {
    pub fn start<F>(device: Option<String>, on_frame: F) -> Result<Self>
    where
        F: FnMut(&[f32]) + Send + 'static,
    {
        let stop = Arc::new(AtomicBool::new(false));
        let (ready_tx, ready_rx) = mpsc::channel::<Result<(), String>>();
        let stop_thread = stop.clone();

        let handle = std::thread::Builder::new()
            .name("native-pw-capture".into())
            .spawn(move || {
                let r = run_capture(&stop_thread, device, on_frame, &ready_tx);
                if let Err(e) = r {
                    let _ = ready_tx.send(Err(e.to_string()));
                }
            })?;

        match ready_rx.recv() {
            Ok(Ok(())) => Ok(Self { stop, handle: Some(handle) }),
            Ok(Err(e)) => bail!("native pw capture init: {e}"),
            Err(_) => bail!("native pw capture thread exited before init"),
        }
    }
}

/// Build the F32/48k/mono format param pod for `connect`.
fn audio_format_param() -> Vec<u8> {
    let mut info = AudioInfoRaw::new();
    info.set_format(AudioFormat::F32LE);
    info.set_rate(SAMPLE_RATE);
    info.set_channels(1);
    let obj = Object {
        type_: spa::utils::SpaTypes::ObjectParamFormat.as_raw(),
        id: spa::param::ParamType::EnumFormat.as_raw(),
        properties: info.into(),
    };
    let values = PodSerializer::serialize(Cursor::new(Vec::new()), &Value::Object(obj))
        .unwrap()
        .0
        .into_inner();
    values
}

fn run_capture<F>(
    stop: &AtomicBool,
    device: Option<String>,
    on_frame: F,
    ready: &mpsc::Sender<Result<(), String>>,
) -> Result<()>
where
    F: FnMut(&[f32]) + Send + 'static,
{
    pw::init();
    let mainloop = pw::main_loop::MainLoop::new(None)?;
    let context = pw::context::Context::new(&mainloop)?;
    let core = context.connect(None)?;

    let mut props = properties! {
        *pw::keys::MEDIA_TYPE => "Audio",
        *pw::keys::MEDIA_CATEGORY => "Capture",
        *pw::keys::MEDIA_ROLE => "Communication",
        *pw::keys::NODE_LATENCY => quantum_prop(std::env::var("HEARTH_PW_QUANTUM").ok().as_deref()),
        *pw::keys::NODE_NAME => "hearth-voice-capture",
    };
    if let Some(id) = device.filter(|s| !s.is_empty()) {
        props.insert(*pw::keys::TARGET_OBJECT, id);
    }

    let stream = pw::stream::Stream::new(&core, "hearth-voice-capture", props)?;

    // Per-frame mono scratch reused across callbacks.
    let mut mono: Vec<f32> = Vec::with_capacity(SAMPLE_RATE as usize / 100);
    let mut on_frame = on_frame;
    let mut channels = 1usize;

    let _listener = stream
        .add_local_listener_with_user_data(())
        .param_changed(move |_, _, id, param| {
            // Latch the negotiated channel count from the Format param.
            if id != pw::spa::param::ParamType::Format.as_raw() {
                return;
            }
            if let Some(param) = param {
                let mut info = AudioInfoRaw::new();
                if info.parse(param).is_ok() {
                    channels = info.channels().max(1) as usize;
                }
            }
        })
        .process(move |stream, _| {
            while let Some(mut buffer) = stream.dequeue_buffer() {
                let datas = buffer.datas_mut();
                if datas.is_empty() {
                    continue;
                }
                let data = &mut datas[0];
                let n_bytes = data.chunk().size() as usize;
                if let Some(slice) = data.data() {
                    let samples: &[f32] = bytemuck::cast_slice(&slice[..n_bytes]);
                    downmix_to_mono(samples, channels, &mut mono);
                    if !mono.is_empty() {
                        on_frame(&mono);
                    }
                }
            }
        })
        .register()?;

    let param = audio_format_param();
    let mut params = [Pod::from_bytes(&param).unwrap()];
    stream.connect(
        spa::utils::Direction::Input,
        None,
        pw::stream::StreamFlags::AUTOCONNECT
            | pw::stream::StreamFlags::MAP_BUFFERS
            | pw::stream::StreamFlags::RT_PROCESS,
        &mut params,
    )?;

    let _ = ready.send(Ok(()));

    // Pump the loop; quit when stop is set. A timer wakes the loop periodically
    // to re-check the stop flag.
    let loop_ref = mainloop.loop_();
    let timer = loop_ref.add_timer(move |_| {});
    timer.update_timer(Some(std::time::Duration::from_millis(100)),
                       Some(std::time::Duration::from_millis(100))).into_result()?;
    while !stop.load(Ordering::Relaxed) {
        mainloop.loop_().iterate(std::time::Duration::from_millis(100));
    }
    Ok(())
}
```

> Implementation notes (verify against `cargo build`; pipewire-rs 0.8 names may need minor adjustment):
> - Add `bytemuck = "1"` to `engine/Cargo.toml` if not present (used for `cast_slice`).
> - The exact loop-driving idiom (`iterate` vs running the mainloop with an external quit signal) may need to use `pw::main_loop::MainLoop` `run()`/`quit()` with a channel; the goal is: pump until `stop`, then return so the thread joins cleanly. If `iterate` is unavailable, use a `pw::channel` to post a quit from `Drop`.
> - `AudioInfoRaw::into()` producing `properties` for the `Object` may instead require `info.into()` returning a `Vec<Property>`; consult the `audio-capture` example in the pipewire-rs repo.

- [ ] **Step 2: Build**

Run: `cargo build -p hearth-engine`
Expected: PASS. Resolve any pipewire-rs 0.8 API name drift here (the structure is correct; method names are the only likely fixes).

- [ ] **Step 3: Commit**

```bash
git add engine/src/audio/native_pw.rs engine/Cargo.toml
git commit -m "feat(voice): native PipeWire capture stream (pinned quantum, mono f32)"
```

---

### Task 5: PipeWire playback stream + mixer + far-end tap

**Files:**
- Modify: `engine/src/audio/native_pw.rs`

**Interfaces:**
- Consumes: `enqueue_trim`, `push_far`, `MAX_LANE_SAMPLES`, `FAR_END_CAP`, `SAMPLE_RATE`, and `crate::audio::native::soft_clip`.
- Produces: a working `NativePlayback::start` whose render callback sums lanes, soft-clips, writes the output buffer, and taps the rendered mono into `far_end`.

- [ ] **Step 1: Implement the playback stream**

Replace the stubbed `NativePlayback::start`. Remove the `#[allow(dead_code)]` added in Task 3.

```rust
impl NativePlayback {
    pub fn start(device: Option<String>) -> Result<Self> {
        let stop = Arc::new(AtomicBool::new(false));
        let sources: Arc<Mutex<HashMap<u64, VecDeque<f32>>>> = Arc::new(Mutex::new(HashMap::new()));
        let far_end: Arc<Mutex<VecDeque<f32>>> = Arc::new(Mutex::new(VecDeque::new()));
        let (ready_tx, ready_rx) = mpsc::channel::<Result<(), String>>();

        let stop_t = stop.clone();
        let sources_t = sources.clone();
        let far_t = far_end.clone();
        let handle = std::thread::Builder::new()
            .name("native-pw-playback".into())
            .spawn(move || {
                let r = run_playback(&stop_t, device, &sources_t, &far_t, &ready_tx);
                if let Err(e) = r {
                    let _ = ready_tx.send(Err(e.to_string()));
                }
            })?;

        match ready_rx.recv() {
            Ok(Ok(())) => Ok(Self { stop, sources, far_end, handle: Some(handle) }),
            Ok(Err(e)) => bail!("native pw playback init: {e}"),
            Err(_) => bail!("native pw playback thread exited before init"),
        }
    }
    // far_end / push / remove_source unchanged from Task 3.
}

fn run_playback(
    stop: &AtomicBool,
    device: Option<String>,
    sources: &Arc<Mutex<HashMap<u64, VecDeque<f32>>>>,
    far_end: &Arc<Mutex<VecDeque<f32>>>,
    ready: &mpsc::Sender<Result<(), String>>,
) -> Result<()> {
    pw::init();
    let mainloop = pw::main_loop::MainLoop::new(None)?;
    let context = pw::context::Context::new(&mainloop)?;
    let core = context.connect(None)?;

    let mut props = properties! {
        *pw::keys::MEDIA_TYPE => "Audio",
        *pw::keys::MEDIA_CATEGORY => "Playback",
        *pw::keys::MEDIA_ROLE => "Communication",
        *pw::keys::NODE_LATENCY => quantum_prop(std::env::var("HEARTH_PW_QUANTUM").ok().as_deref()),
        *pw::keys::NODE_NAME => "hearth-voice-playback",
    };
    if let Some(id) = device.filter(|s| !s.is_empty()) {
        props.insert(*pw::keys::TARGET_OBJECT, id);
    }

    let stream = pw::stream::Stream::new(&core, "hearth-voice-playback", props)?;
    let sources_cb = sources.clone();
    let far_cb = far_end.clone();

    let _listener = stream
        .add_local_listener_with_user_data(())
        .process(move |stream, _| {
            while let Some(mut buffer) = stream.dequeue_buffer() {
                let datas = buffer.datas_mut();
                if datas.is_empty() {
                    continue;
                }
                let data = &mut datas[0];
                let stride = std::mem::size_of::<f32>();
                if let Some(slice) = data.data() {
                    let n_frames = slice.len() / stride;
                    let out: &mut [f32] = bytemuck::cast_slice_mut(slice);
                    let mut src = sources_cb.lock().unwrap();
                    let mut far = far_cb.lock().unwrap();
                    for o in out.iter_mut().take(n_frames) {
                        let mut v = 0.0f32;
                        for q in src.values_mut() {
                            if let Some(s) = q.pop_front() {
                                v += s;
                            }
                        }
                        v = crate::audio::native::soft_clip(v);
                        *o = v;
                        far.push_back(v);
                    }
                    while far.len() > FAR_END_CAP {
                        far.pop_front();
                    }
                    let chunk = data.chunk_mut();
                    *chunk.size_mut() = (n_frames * stride) as u32;
                    *chunk.stride_mut() = stride as i32;
                }
            }
        })
        .register()?;

    let param = audio_format_param(); // mono F32 48k — playback negotiates mono too
    let mut params = [Pod::from_bytes(&param).unwrap()];
    stream.connect(
        spa::utils::Direction::Output,
        None,
        pw::stream::StreamFlags::AUTOCONNECT
            | pw::stream::StreamFlags::MAP_BUFFERS
            | pw::stream::StreamFlags::RT_PROCESS,
        &mut params,
    )?;

    let _ = ready.send(Ok(()));
    while !stop.load(Ordering::Relaxed) {
        mainloop.loop_().iterate(std::time::Duration::from_millis(100));
    }
    Ok(())
}
```

> Notes: a mono output node lets PipeWire up-mix to the device. If a mono output negotiation is rejected by the server, request the device's channel count in `audio_format_param` for playback and write the mono value to every channel (mirror the WASAPI `out[f*ch+c]=v` loop). Verify by ear during manual test.

- [ ] **Step 2: Build**

Run: `cargo build -p hearth-engine`
Expected: PASS.

- [ ] **Step 3: Commit**

```bash
git add engine/src/audio/native_pw.rs
git commit -m "feat(voice): native PipeWire playback mixer + AEC far-end tap"
```

---

### Task 6: Make `native_voice.rs` cross-platform

**Files:**
- Modify: `engine/src/audio/native_voice.rs` (the top-of-module gate and the `eprintln!` banner)
- Modify: `engine/src/audio/mod.rs` (already widened in Task 2; confirm)

**Interfaces:**
- Consumes: cfg-selected `NativeCapture`/`NativePlayback` from `native.rs`.
- Produces: `NativeVoice` compiling and constructing on Linux.

- [ ] **Step 1: Widen the module gate**

`native_voice.rs` currently relies on being `#[cfg(windows)]` via `mod.rs`. It uses only platform-independent APIs. No per-line cfg is needed inside. Ensure `mod.rs` has:

```rust
#[cfg(any(target_os = "windows", target_os = "linux"))]
pub mod native_voice;
```

- [ ] **Step 2: Make the startup banner platform-accurate**

Replace the hardcoded WASAPI banner near the top of `NativeVoice::new`:

```rust
#[cfg(windows)]
eprintln!("[native-voice] active — WASAPI IAudioClient3 capture/playback + Opus + UDP");
#[cfg(target_os = "linux")]
eprintln!("[native-voice] active — PipeWire capture/playback + Opus + UDP");
```

- [ ] **Step 3: Build**

Run: `cargo build -p hearth-engine`
Expected: PASS on Linux (NativeVoice now compiles against the PipeWire backend).

- [ ] **Step 4: Commit**

```bash
git add engine/src/audio/native_voice.rs engine/src/audio/mod.rs
git commit -m "feat(voice): compile native voice stack on Linux (PipeWire backend)"
```

---

### Task 7: Session selection + automatic fallback

Widen the selection seam to Linux and turn native-construction failure into a fall-through to GStreamer (rather than a fatal error), on both platforms.

**Files:**
- Modify: `engine/src/session.rs` (the `native_voice_selected`/`ns_wet_permille` gates, the `native_voice`/`native_monitor` fields, `ensure_native_voice`, `rebuild_native_voice`, and the offer/answer/stop/device-change call sites)
- Test: inline `#[cfg(test)] mod tests` in `session.rs` for the pure selection helper

**Interfaces:**
- Consumes: `NativeVoice`, `VoiceBackendKind` (Task 8 defines the enum; to avoid a cross-task dependency, define `VoiceBackendKind` here in Task 7 — see Step 4 — and Task 8 only adds the event + UI).
- Produces: `fn pick_backend(force_generic: bool, native_failed: bool) -> bool` (true = use native), a `native_voice_failed: bool` session field, and fall-through behavior.

- [ ] **Step 1: Write the failing selection test**

```rust
#[cfg(test)]
mod backend_tests {
    use super::pick_backend;

    #[test]
    fn native_by_default() {
        assert!(pick_backend(false, false));
    }
    #[test]
    fn forced_generic_skips_native() {
        assert!(!pick_backend(true, false));
    }
    #[test]
    fn prior_failure_stays_generic() {
        assert!(!pick_backend(false, true));
    }
}
```

- [ ] **Step 2: Run it to verify failure**

Run: `cargo test -p hearth-engine --lib backend_tests`
Expected: FAIL (`pick_backend` not defined).

- [ ] **Step 3: Implement `pick_backend` and widen the selector gates**

Replace the `#[cfg(windows)]` on `native_voice_selected` and `ns_wet_permille` with `#[cfg(any(target_os = "windows", target_os = "linux"))]`. Add:

```rust
/// Use native when not force-disabled and no native attempt has failed this
/// session. Pure so it is unit-tested without devices.
#[cfg(any(target_os = "windows", target_os = "linux"))]
pub(crate) fn pick_backend(force_generic: bool, native_failed: bool) -> bool {
    !force_generic && !native_failed
}
```

- [ ] **Step 4: Widen the session fields and add the failure latch**

For every `#[cfg(target_os = "windows")]` on `native_voice`, `native_monitor`, `ensure_native_voice`, `rebuild_native_voice`, and the offer/answer/stop/device-change blocks, change to `#[cfg(any(target_os = "windows", target_os = "linux"))]`. Add a field next to `native_voice`:

```rust
#[cfg(any(target_os = "windows", target_os = "linux"))]
native_voice_failed: bool,
```

initialized `false` in both constructors.

Define the backend kind here (used by Task 8):

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VoiceBackendKind {
    Native,
    Generic,
}
```

- [ ] **Step 5: Make `ensure_native_voice` set the latch, and call sites fall through**

In `ensure_native_voice`, on `NativeVoice::new(...)` error, set `self.native_voice_failed = true`, emit `SessionEvent::Warning(...)`, and return `None`. Replace the `native_voice_selected()` guard at its top with `pick_backend(!native_voice_selected(), self.native_voice_failed)` inverted appropriately — concretely:

```rust
#[cfg(any(target_os = "windows", target_os = "linux"))]
fn ensure_native_voice(&mut self) -> Option<&mut crate::audio::native_voice::NativeVoice> {
    let force_generic = !native_voice_selected();
    if !pick_backend(force_generic, self.native_voice_failed) {
        return None;
    }
    if self.native_voice.is_none() {
        match crate::audio::native_voice::NativeVoice::new(/* …existing args… */) {
            Ok(nv) => self.native_voice = Some(nv),
            Err(e) => {
                self.native_voice_failed = true;
                self.emit(SessionEvent::Warning(format!(
                    "native audio backend unavailable, using generic: {e}"
                )));
                return None;
            }
        }
    }
    self.native_voice.as_mut()
}
```

In `voice_offer` and `voice_on_offer`, the existing pattern is:

```rust
#[cfg(any(target_os = "windows", target_os = "linux"))]
if native_voice_selected() {
    if let Some(nv) = self.ensure_native_voice() {
        // …native add_peer / endpoint / send Offer…
        return Ok(());
    }
    // native unavailable → fall through to the GStreamer branch below
}
// existing GStreamer voice_udp branch (unchanged)
```

Change the guard from `if native_voice_selected()` to `if !self.native_voice_failed && native_voice_selected()` and replace the `.ok_or_else(...)?` error path with the `if let Some(nv) = … { … return }` fall-through shown above. The GStreamer branch then runs when native is unavailable.

- [ ] **Step 6: Run tests + build**

Run: `cargo test -p hearth-engine --lib backend_tests`
Expected: PASS.
Run: `cargo build -p hearth-engine`
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add engine/src/session.rs
git commit -m "feat(voice): native-by-default with auto fall-through to GStreamer"
```

---

### Task 8: Emit and display the active voice backend

**Files:**
- Modify: `engine/src/session.rs` (emit `SessionEvent::VoiceBackend` at the selection site; add the enum variant)
- Modify: `engine/src/lib.rs` (bridge the event to a `WorkspaceInput`)
- Modify: `desktop/src/app.rs` (handle the new bridged event)
- Modify: `desktop/src/ui/settings.rs` (render the read-only "Audio engine" line)

**Interfaces:**
- Consumes: `VoiceBackendKind` (defined in Task 7).
- Produces: `SessionEvent::VoiceBackend(VoiceBackendKind)`, a `WorkspaceInput::VoiceBackend(...)` (name to match the existing input enum style, e.g. matching `SelfSpeaking`), and a settings label.

- [ ] **Step 1: Add the event variant**

In `engine/src/session.rs`, in `pub enum SessionEvent`, add:

```rust
/// Which voice device backend is live this session (native vs generic), so the
/// UI can show the auto-fallback result instead of it being silent.
VoiceBackend(VoiceBackendKind),
```

- [ ] **Step 2: Emit it at the single selection site**

In `voice_offer`/`voice_on_offer`, immediately after deciding the path, emit once per session (guard with a `bool` field `voice_backend_announced` to avoid spamming; reset it in `rebuild_native_voice`):

```rust
let kind = if used_native { VoiceBackendKind::Native } else { VoiceBackendKind::Generic };
if !self.voice_backend_announced {
    self.voice_backend_announced = true;
    self.emit(SessionEvent::VoiceBackend(kind));
}
```

where `used_native` is `true` in the native arm (after a successful `ensure_native_voice`) and `false` in the GStreamer arm. Add `voice_backend_announced: bool` (init `false`) and reset to `false` in `rebuild_native_voice` and `stop_voice` when the last peer leaves.

- [ ] **Step 3: Bridge in `lib.rs`**

Find where `SessionEvent::SelfSpeaking` is matched in `engine/src/lib.rs` (the engine→desktop bridge) and add a sibling arm:

```rust
SessionEvent::VoiceBackend(kind) => {
    let label = match kind {
        crate::session::VoiceBackendKind::Native => "native",
        crate::session::VoiceBackendKind::Generic => "generic",
    };
    // forward via the same channel/notification mechanism used by SelfSpeaking
    // e.g. emit an FFI/bridge event "voice_backend" with `label`
}
```

> Match the exact bridge mechanism in `lib.rs` (it mirrors the `n*` events seen in `desktop/src/app.rs`, e.g. a `Notification::VoiceBackend`). Add the corresponding variant to that bridge enum.

- [ ] **Step 4: Handle it in the desktop app**

In `desktop/src/app.rs`, alongside `nSelfSpeaking(on) =>`, add `nVoiceBackend(label) =>` that sends `WorkspaceInput::VoiceBackend(label)` (add that `WorkspaceInput` variant where the enum is defined).

- [ ] **Step 5: Render in Settings**

In `desktop/src/ui/settings.rs`, store the latest backend string in the settings model and render a read-only line in the voice section, following the existing read-only status-line pattern (the DSP-profile / RT-probe surfaces):

```rust
// "Audio engine: Native (PipeWire)" / "Audio engine: Generic (GStreamer)"
let engine = match self.voice_backend.as_deref() {
    Some("native") if cfg!(target_os = "linux") => "Native (PipeWire)",
    Some("native") => "Native (WASAPI)",
    Some("generic") => "Generic (GStreamer)",
    _ => "—",
};
```

- [ ] **Step 6: Build the workspace**

Run: `cargo build`
Expected: PASS (engine + desktop).

- [ ] **Step 7: Commit**

```bash
git add engine/src/session.rs engine/src/lib.rs desktop/src/app.rs desktop/src/ui/settings.rs
git commit -m "feat(voice): surface active audio backend (native/generic) in Settings"
```

---

### Task 9: Drift-verification logging + docs

**Files:**
- Modify: `engine/src/audio/native_pw.rs` (period + lane-backlog logging)
- Modify: `docs/findings/voice-latency-linux.md` (how to verify the drift fix)

**Interfaces:**
- Produces: periodic `[native]` logs of the capture period and deepest mixer-lane backlog.

- [ ] **Step 1: Add a capture-period log**

In `run_capture`'s `process`, count callbacks and every ~200 frames log the observed frame size and derived period:

```rust
// once per ~2 s: confirm the quantum stays pinned (no drift)
cb_count += 1;
if cb_count % 200 == 0 {
    eprintln!("[native] capture period: {} samples ({:.1} ms)",
              mono.len(), mono.len() as f64 / SAMPLE_RATE as f64 * 1000.0);
}
```

(declare `let mut cb_count = 0u64;` in `run_capture` before the listener).

- [ ] **Step 2: Add a playback lane-backlog log**

In `run_playback`'s `process`, every ~200 callbacks log the deepest lane (mirrors the WASAPI `[native] playback lane backlog` line) so a long-session call shows bounded backlog.

- [ ] **Step 3: Document the verification**

Append to `docs/findings/voice-latency-linux.md`:

```markdown
## Verifying the native PipeWire drift fix
1. Run a call (native is default; `[native-voice] active — PipeWire …` confirms it).
2. Watch `[native] capture period` — it must stay near the pinned quantum
   (256/48000 ≈ 5.3 ms) and not grow over a 30+ min session.
3. Watch `[native] playback lane backlog` — must stay bounded (~≤20 ms).
4. Force the generic path with `HEARTH_GSTREAMER_VOICE=1` to compare; the old
   pulsesrc path drifts toward ~70 ms over a long session.
```

- [ ] **Step 4: Build**

Run: `cargo build -p hearth-engine`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add engine/src/audio/native_pw.rs docs/findings/voice-latency-linux.md
git commit -m "feat(voice): drift-verification logging for native PipeWire path"
```

---

## Self-Review

**Spec coverage:**
- Native pipewire-rs backend → Tasks 3–5. ✓
- Module split (`native_wasapi.rs` / `native_pw.rs` / shared umbrella) → Task 2. ✓
- `native_voice.rs` cross-platform → Task 6. ✓
- Pinned quantum + `HEARTH_PW_QUANTUM` → Task 3 (`quantum_prop`), Tasks 4–5 (applied). ✓
- Device mapping via `target.object` → Tasks 4–5. ✓
- AEC far-end from playback ring → Task 5. ✓
- Default-on + auto-fallback + `HEARTH_GSTREAMER_VOICE` override → Task 7. ✓
- Backend indicator in UI → Task 8. ✓
- Dependency + build dep → Task 1. ✓
- Drift verification → Task 9. ✓

**Placeholder scan:** Device-I/O steps that cannot be unit-tested state the real reason (needs live server) and give the build+manual cycle; pipewire-rs API-name caveats are explicit, not vague TODOs. No "add error handling"/"TBD" requirements remain.

**Type consistency:** `NativeCapture::start(Option<String>, FnMut(&[f32]))`, `NativePlayback::{start, push, far_end, remove_source}`, `downmix_to_mono`, `enqueue_trim`, `push_far`, `quantum_prop`, `pick_backend`, `VoiceBackendKind{Native,Generic}` are used with identical signatures across tasks.

**Known risk to resolve at implementation:** pipewire-rs 0.8 exact method names (`dequeue_buffer`, `datas_mut`, `chunk`, loop driving) — verified structurally against the official `audio-capture` example; fix names against `cargo build` in Tasks 4–5.
</content>
