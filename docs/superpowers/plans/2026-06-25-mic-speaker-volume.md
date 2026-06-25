# Mic/Speaker Volume Wiring Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the persisted mic (input) and speaker (output) volume sliders actually attenuate live audio on both the native and GStreamer voice paths.

**Architecture:** A single linear gain applied at the existing f32 mono-frame points — mic = pre-amp at the front of capture (before AEC), speaker = gain on the rendered mix (master). `Session` stores both volumes and forwards them live to whichever path is active; the desktop calls the setters from the slider handlers and at startup.

**Tech Stack:** Rust, existing audio path (`native_voice`/`native_pw`/`native_wasapi`, GStreamer `capture`/`voice_udp`), GTK4 desktop.

## Global Constraints

- Volume range **attenuate-only `[0.0, 1.0]`**, unity at 1.0; clamp in the `Session` setters. No boost/soft-clip.
- Applied **live** (no rebuild); re-applied after a device-change rebuild and on new peers.
- Both backends: native (default) **and** GStreamer `voice_udp` fallback.
- Volumes stored as `Arc<AtomicU32>` holding `f32::to_bits` (read with `f32::from_bits`).
- `native_wasapi.rs` is `cfg(windows)` and cannot be built/tested on the Linux dev box — mirror the `native_pw.rs` change; verify it compiles on Windows.
- Commit messages: no Claude attribution. En dashes, not em dashes.
- Don't auto-launch the app; manual device verification is the user's.

---

### Task 1: `apply_gain` helper (TDD)

**Files:**
- Modify: `engine/src/audio/capture.rs` (add helper + tests near `i16_to_f32`)

**Interfaces:**
- Produces: `pub(crate) fn apply_gain(frame: &mut [f32], gain: f32)` — multiplies every sample in place.

- [ ] **Step 1: Write the failing tests**

Add to the `#[cfg(test)] mod tests` in `engine/src/audio/capture.rs`:

```rust
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
```

- [ ] **Step 2: Run it, verify it fails**

Run: `cargo test -p engine --lib apply_gain_identity_half_zero`
Expected: FAIL — `cannot find function apply_gain`.

- [ ] **Step 3: Implement the helper**

Add near the other f32 helpers in `engine/src/audio/capture.rs`:

```rust
/// Scale every sample in place by `gain` (a linear multiplier). Used for the
/// user mic/speaker volume; `gain` is expected pre-clamped to `[0.0, 1.0]`.
pub(crate) fn apply_gain(frame: &mut [f32], gain: f32) {
    for s in frame.iter_mut() {
        *s *= gain;
    }
}
```

- [ ] **Step 4: Run it, verify it passes**

Run: `cargo test -p engine --lib apply_gain_identity_half_zero`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add engine/src/audio/capture.rs
git commit -m "feat(voice): apply_gain helper for user volume"
```

---

### Task 2: Native path — input pre-amp + output volume

**Files:**
- Modify: `engine/src/audio/native_voice.rs` (input pre-amp + `NativeVoice::set_input_volume`/`set_output_volume`)
- Modify: `engine/src/audio/native/native_pw.rs` (`NativePlayback` output volume)
- Modify: `engine/src/audio/native/native_wasapi.rs` (mirror the `NativePlayback` change; Windows-only, verify there)

**Interfaces:**
- Consumes: `crate::audio::capture::apply_gain`.
- Produces: `NativePlayback::set_volume(&self, v: f64)`; `NativeVoice::set_input_volume(&self, v: f64)`, `NativeVoice::set_output_volume(&self, v: f64)`.

- [ ] **Step 1: Add the output-volume atomic to `NativePlayback` (native_pw)**

In `engine/src/audio/native/native_pw.rs`, add a field to `NativePlayback`:

```rust
    far_end: Arc<Mutex<VecDeque<f32>>>,
    /// Master speaker volume (f32 bits, 0.0–1.0), applied in the render loop.
    volume: Arc<AtomicU32>,
    handle: Option<std::thread::JoinHandle<()>>,
```

Add `use std::sync::atomic::AtomicU32;` if not already imported (it imports `AtomicBool`/`Ordering` already — extend it).

In `start()`, create and thread it through:

```rust
        let volume = Arc::new(AtomicU32::new(1.0f32.to_bits()));
        let volume_t = volume.clone();
        // ... in the thread spawn, pass volume_t into run_playback ...
```

Change `run_playback`'s signature to take `volume: Arc<AtomicU32>` and move it into the `process` closure. In the render loop, read it once per callback and scale before `soft_clip`:

```rust
        .process(move |stream, _| {
            let vol = f32::from_bits(volume.load(Ordering::Relaxed));
            // ... existing dequeue/datas ...
                    for frame in out.chunks_exact_mut(CHANNELS) {
                        let mut v = 0.0f32;
                        for q in src.values_mut() {
                            if let Some(s) = q.pop_front() {
                                v += s;
                            }
                        }
                        v = crate::audio::native::soft_clip(v * vol); // master volume, then limiter
                        for ch in frame.iter_mut() {
                            *ch = v;
                        }
                        rendered.push(v);
                    }
            // ...
        })
```

Store `volume` in the returned `Self { … volume, … }`, and add the setter:

```rust
    /// Set master speaker volume (0.0–1.0). Live.
    pub fn set_volume(&self, v: f64) {
        self.volume.store((v as f32).to_bits(), Ordering::Relaxed);
    }
```

- [ ] **Step 2: Mirror the same change in `native_wasapi.rs`**

In `engine/src/audio/native/native_wasapi.rs`, add the identical `volume: Arc<AtomicU32>` field + `set_volume`, and in `playback_loop` scale the mixed sample before `soft_clip`:

```rust
                v = soft_clip(v * vol); // vol read once per callback: f32::from_bits(volume.load(Relaxed))
```

(Read `let vol = f32::from_bits(volume.load(Ordering::Relaxed));` once at the top of the write block; pass `volume` into `playback_loop` like `native_pw`.) Cannot build locally — verify on Windows.

- [ ] **Step 3: Add the input pre-amp + setters in `native_voice.rs`**

Add an input-volume atomic next to the other capture-thread state (near `ns_wet`):

```rust
        let input_volume = Arc::new(AtomicU32::new(1.0f32.to_bits()));
        let input_vol_cb = input_volume.clone();
```

At the very top of the `NativeCapture::start(input_device, move |mono| { … })` closure, pre-amp the mic before anything else:

```rust
        let capture = NativeCapture::start(input_device, move |mono| {
            // Mic pre-amp (user input volume) before AEC/DSP.
            let in_vol = f32::from_bits(input_vol_cb.load(Ordering::Relaxed));
            let mut in_scaled: Vec<f32> = Vec::new();
            let mono: &[f32] = if (in_vol - 1.0).abs() > f32::EPSILON {
                in_scaled.extend_from_slice(mono);
                crate::audio::capture::apply_gain(&mut in_scaled, in_vol);
                &in_scaled
            } else {
                mono
            };

            let ec_on = ec_cb.load(Ordering::Relaxed);
            // ... rest unchanged ...
```

Add `input_volume` to the `NativeVoice` struct and the returned `Self { … }`. Add the setters:

```rust
    /// Set mic input volume (0.0–1.0). Live.
    pub fn set_input_volume(&self, v: f64) {
        self.input_volume.store((v as f32).to_bits(), Ordering::Relaxed);
    }

    /// Set master speaker volume (0.0–1.0). Live.
    pub fn set_output_volume(&self, v: f64) {
        self.playback.set_volume(v);
    }
```

- [ ] **Step 4: Build (Linux native path)**

Run: `cargo build -p engine`
Expected: PASS (no warnings beyond the pre-existing `set_speaking`).

- [ ] **Step 5: Commit**

```bash
git add engine/src/audio/native_voice.rs engine/src/audio/native/native_pw.rs engine/src/audio/native/native_wasapi.rs
git commit -m "feat(voice): native mic pre-amp + master speaker volume"
```

---

### Task 3: GStreamer fallback — capture mic gain + per-peer recv volume

**Files:**
- Modify: `engine/src/audio/capture.rs` (`VoiceCapture` mic gain + `set_input_volume`)
- Modify: `engine/src/voice_udp.rs` (`VoiceUdpPeer` recv `volume` element + `set_output_volume`)

**Interfaces:**
- Consumes: `apply_gain`.
- Produces: `VoiceCapture::set_input_volume(&self, v: f64)`; `VoiceUdpPeer::set_output_volume(&self, v: f64)`.

- [ ] **Step 1: Mic gain in `VoiceCapture` (capture.rs)**

`VoiceCapture` already has `Arc<Mutex<…>>` shared state and a `cap` appsink callback that owns the `Dsp`. Add a shared input-gain atomic. In `VoiceCapture`:

```rust
    input_volume: Arc<std::sync::atomic::AtomicU32>,
```

Create it in `start()` (`Arc::new(AtomicU32::new(1.0f32.to_bits()))`), clone into `build_mic_pipeline`, and in the `cap` callback pre-amp the decoded `mic` frame **before** `dsp.process_capture`:

```rust
                let in_vol = f32::from_bits(input_volume_cb.load(Ordering::Relaxed));
                if (in_vol - 1.0).abs() > f32::EPSILON {
                    apply_gain(&mut mic, in_vol);
                }
                let vad = dsp.process_capture(&mut mic);
```

Add the setter:

```rust
    pub fn set_input_volume(&self, v: f64) {
        self.input_volume.store((v as f32).to_bits(), std::sync::atomic::Ordering::Relaxed);
    }
```

- [ ] **Step 2: Recv `volume` element in `VoiceUdpPeer` (voice_udp.rs)**

In `VoiceUdpPeer::new`, insert a `volume` element into the recv chain between `rresample` and `spk_valve`:

```rust
        let rvolume = gst::ElementFactory::make("volume")
            .name("spk_volume")
            .property("volume", 1.0f64)
            .build()?;
```

Add it to `add_many` and `link_many` (… `rresample, &rvolume, &spk_valve, &sink`). Store `rvolume` in the struct and add:

```rust
    /// Master speaker volume for this peer's playback (0.0–1.0). Live.
    pub fn set_output_volume(&self, v: f64) {
        self.spk_volume.set_property("volume", v);
    }
```

(name the field `spk_volume`).

- [ ] **Step 3: Build**

Run: `cargo build -p engine`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add engine/src/audio/capture.rs engine/src/voice_udp.rs
git commit -m "feat(voice): GStreamer fallback mic gain + per-peer speaker volume"
```

---

### Task 4: Session wiring + desktop

**Files:**
- Modify: `engine/src/session.rs` (fields, setters, apply on add-peer/rebuild/ensure)
- Modify: `desktop/src/app.rs` (call setters from handlers + startup)
- Test: inline `#[cfg(test)] mod` in `session.rs` for the clamp

**Interfaces:**
- Consumes: `NativeVoice::set_input_volume`/`set_output_volume`, `VoiceCapture::set_input_volume`, `VoiceUdpPeer::set_output_volume`.
- Produces: `Session::set_input_volume(&mut self, v: f64)`, `Session::set_output_volume(&mut self, v: f64)`, `fn clamp_volume(v: f64) -> f64`.

- [ ] **Step 1: Write the failing clamp test**

In `engine/src/session.rs`:

```rust
#[cfg(test)]
mod volume_tests {
    use super::clamp_volume;

    #[test]
    fn clamps_to_unit_range() {
        assert_eq!(clamp_volume(-0.2), 0.0);
        assert_eq!(clamp_volume(0.5), 0.5);
        assert_eq!(clamp_volume(1.0), 1.0);
        assert_eq!(clamp_volume(2.5), 1.0);
    }
}
```

- [ ] **Step 2: Run it, verify it fails**

Run: `cargo test -p engine --lib volume_tests`
Expected: FAIL — `cannot find function clamp_volume`.

- [ ] **Step 3: Implement clamp + fields + setters**

Add the free fn:

```rust
/// User volume sliders are attenuate-only; clamp to the unit range.
pub(crate) fn clamp_volume(v: f64) -> f64 {
    v.clamp(0.0, 1.0)
}
```

Add to the `Session` struct (near `dsp_config`):

```rust
    input_volume: f64,
    output_volume: f64,
```

Initialize `input_volume: 1.0, output_volume: 1.0` in **both** `Session { … }` constructors.

Add the setters (non-cfg; they no-op when a path isn't active):

```rust
    /// Set mic input volume (0.0–1.0), live, on whichever voice path is active.
    pub fn set_input_volume(&mut self, v: f64) {
        let v = clamp_volume(v);
        self.input_volume = v;
        #[cfg(any(target_os = "windows", target_os = "linux"))]
        if let Some(nv) = self.native_voice.as_ref() {
            nv.set_input_volume(v);
        }
        if let Some(vc) = self.voice_capture.as_ref() {
            vc.set_input_volume(v);
        }
    }

    /// Set master speaker volume (0.0–1.0), live, on whichever voice path is active.
    pub fn set_output_volume(&mut self, v: f64) {
        let v = clamp_volume(v);
        self.output_volume = v;
        #[cfg(any(target_os = "windows", target_os = "linux"))]
        if let Some(nv) = self.native_voice.as_ref() {
            nv.set_output_volume(v);
        }
        for p in self.voice_peers.values() {
            p.set_output_volume(v);
        }
    }
```

- [ ] **Step 4: Re-apply stored volumes to new peers / rebuilt instances**

In `ensure_native_voice`, right after `self.native_voice = Some(nv);`, apply the stored volumes:

```rust
                Ok(nv) => {
                    self.native_voice = Some(nv);
                    if let Some(nv) = self.native_voice.as_ref() {
                        nv.set_input_volume(self.input_volume);
                        nv.set_output_volume(self.output_volume);
                    }
                }
```

In the GStreamer `voice_offer` / `voice_on_offer` arms, after the peer is inserted into `self.voice_peers`, apply output volume to it (mic gain is on the shared `voice_capture`, applied when the capture is (re)built — apply there too). Concretely, after `self.register_voice_send(peer)` in `voice_offer` and `self.register_voice_send(from)` in `voice_on_offer`, add:

```rust
        if let Some(p) = self.voice_peers.get(&peer) { // or &from
            p.set_output_volume(self.output_volume);
        }
```

In `restart_voice_capture`, after building `VoiceCapture`, apply `vc.set_input_volume(self.input_volume);` before storing it.

- [ ] **Step 5: Run the clamp test + build**

Run: `cargo test -p engine --lib volume_tests`
Expected: PASS.
Run: `cargo build -p engine`
Expected: PASS.

- [ ] **Step 6: Desktop — call the setters**

In `desktop/src/app.rs`, change the volume handlers to apply live and persist:

```rust
            SettingsOutput::InputVolume(v) => {
                settings.input_volume = v;
                if let Some(s) = self.session.as_mut() {
                    s.set_input_volume(v);
                }
            }
            SettingsOutput::OutputVolume(v) => {
                settings.output_volume = v;
                if let Some(s) = self.session.as_mut() {
                    s.set_output_volume(v);
                }
            }
```

In `apply_settings_to_session` (startup + device-change), add after the existing calls:

```rust
        session.set_input_volume(s.input_volume);
        session.set_output_volume(s.output_volume);
```

(`apply_settings_to_session` takes `session: &mut Session` — confirm it's `&mut`; the setters need `&mut self`. The InputVolume/OutputVolume handlers fall through to the common save path, so persistence is preserved.)

- [ ] **Step 7: Build the workspace**

Run: `cargo build`
Expected: PASS (engine + desktop).

- [ ] **Step 8: Commit**

```bash
git add engine/src/session.rs desktop/src/app.rs
git commit -m "feat(voice): wire mic/speaker volume sliders to the live session"
```

---

## Manual verification (user, after Task 4)

Relaunch (`pkill -x desktop; scripts/dev/launch-test.sh --debug`), then in Settings → Voice:
- Drag **Mic volume** down mid-call → the other peer hears you quieter; at 0 you're silent. Mic test meter reflects it.
- Drag **Speaker vol.** down → incoming audio gets quieter; at 0 silent.
- Both apply **immediately** (no rejoin), and persist across a relaunch.
- Force the fallback with `HEARTH_GSTREAMER_VOICE=1` and re-check both sliders.

## Self-Review

**Spec coverage:** range/clamp (Task 4 `clamp_volume`), both gain points (Task 2 native, Task 3 GStreamer), live + rebuild re-apply (Task 4 steps 4/6), desktop wiring (Task 4 step 6), pure-helper test (Task 1) — all covered.

**Placeholder scan:** every code step shows real code; the only "verify on Windows" note is the genuine `cfg(windows)` constraint, not a placeholder.

**Type consistency:** `apply_gain(&mut [f32], f32)`, `set_input_volume`/`set_output_volume`/`set_volume(f64)`, `clamp_volume(f64)->f64`, atomic `f32::to_bits`/`from_bits` — used identically across tasks.
</content>
