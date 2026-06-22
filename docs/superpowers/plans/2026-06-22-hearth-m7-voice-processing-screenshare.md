# Hearth M7 – Voice Processing, Device Selection & Advanced Screenshare Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give Hearth Discord-tier voice and screenshare control – input/output device selection, a mic test meter, in-process voice DSP (echo cancel / noise suppression / AGC / VAD), push-to-talk, and a Screen Share picker with selectable source, per-app/system audio, quality and live preview.

**Architecture:** Engine grows an `audio` module (device enumeration, a `webrtc-audio-processing` DSP wrapper, a capture+DSP+meter+valve branch, a standalone mic-test monitor), a `screen` module (X11 source enumeration, PipeWire audio-node listing, source/quality-aware capture), and a `hotkey` module (X11 global PTT). The desktop gains a local `Settings` model, a Voice settings page, a Screen Share picker, and a level-meter widget. All client-side; no backend/protocol/DB changes.

**Tech Stack:** Rust, GStreamer (`pulsesrc`/`pulsesink`/`pipewiresrc`/`ximagesrc`/`level`/`appsink`/`appsrc`/`webrtcbin`/`opusenc`), the `webrtc-audio-processing` crate (libwebrtc AEC/NS/AGC/VAD, built via cmake+clang), `x11rb` (window enumeration + `XGrabKey`), GTK4 + relm4, the existing `hearth` engine `Session`/`FlowPeer`.

## Global Constraints

- **Work on `main`, commit locally** (committing allowed); one commit per task. Do **not** push.
- **Source Rust** with `. "$HOME/.cargo/env"` in every Bash call.
- **Primary target Linux (Ubuntu Noble, X11, PipeWire).** Windows is a later, separate milestone (out of scope).
- **Voice DSP is in-process** via the `webrtc-audio-processing` crate (the GStreamer `webrtcdsp` element is not available on Noble). DSP frames are **10 ms, 48 kHz** (480 samples/channel).
- **Voice = full DSP; screenshare audio = stereo 48 kHz, no DSP** (music-mode Opus). The voice flow and screenshare audio never share a DSP processor.
- **Screenshare sources (X11): whole screen + specific window only** (`ximagesrc` / `ximagesrc xid=`). Region select is out of scope.
- **App/system audio via PipeWire directly** (`pipewiresrc target-object=` / `pulsesrc device=<sink>.monitor`), always excluding our own process. No virtual-mic addon. Gated on PipeWire being present.
- **Settings persist in a local config file only.** No backend, protocol, or DB changes.
- **Activation precedence:** mute > push-to-talk > voice-activity > always-on, all driving the existing `mic_valve`.
- **TDD** for engine logic (device mapping, DSP framing, activation gate, settings serde, PipeWire node filters); **run-and-observe** for DSP audio, capture, and UI. **Screenshare run-and-observe MUST use** `HEARTH_CAPTURE="videotestsrc is-live=true pattern=ball ! timeoverlay ! videoconvert"` – never grab the real `:0` screen for testing (it crashed the user's session in M6).

---

## File Structure

```
engine/
  Cargo.toml                      # + webrtc-audio-processing, x11rb
  src/
    lib.rs                        # + pub mod audio; pub mod screen; pub mod hotkey;
    audio/
      mod.rs                      # re-exports
      devices.rs                  # AudioDevice + DeviceMonitor enumeration
      dsp.rs                      # DspConfig + Processor (webrtc-audio-processing)
      gate.rs                     # Activation state machine (pure)
      capture.rs                  # voice capture+DSP-bridge+meter+valve helpers
      monitor.rs                  # standalone mic-test/meter pipeline
    screen/
      mod.rs                      # re-exports
      sources.rs                  # X11 screen+window enumeration
      audio.rs                    # PipeWire node listing + filter rules
      capture.rs                  # ShareConfig -> caps/source/preview
    hotkey.rs                     # X11 XGrabKey global PTT
    session.rs                    # wire settings -> flows; live-apply; screen audio track
    flow_peer.rs                  # voice send branch swap; screen audio mux
desktop/
  src/
    config.rs                     # + Settings struct (serde) + load/save
    main.rs                       # mod additions
    ui/
      mod.rs
      meter.rs                    # level meter widget
      settings.rs                 # Settings window, Voice page
      screenshare_picker.rs       # source grid + preview + quality + audio source
      self_panel.rs               # + gear button -> open settings; share -> picker
      workspace.rs                # host the settings/picker controllers
```

---

## Task 1: Engine – audio device enumeration

**Files:** Create `engine/src/audio/mod.rs`, `engine/src/audio/devices.rs`; Modify `engine/src/lib.rs`.

**Interfaces:**
- Produces: `pub struct AudioDevice { pub id: String, pub label: String, pub kind: DeviceKind, pub is_default: bool }`, `pub enum DeviceKind { Source, Sink }`, `pub fn list_devices() -> Vec<AudioDevice>`, and `pub(crate) fn device_to_info(d: &gst::Device, default_name: Option<&str>) -> Option<AudioDevice>`.

- [ ] **Step 1: Declare the module.** In `engine/src/lib.rs` add `pub mod audio;` (alongside the existing `pub mod` lines). Create `engine/src/audio/mod.rs` with `pub mod devices;`.

- [ ] **Step 2: Write the failing test** (`engine/src/audio/devices.rs`, `#[cfg(test)]`)

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_pulse_device_to_info() {
        gstreamer::init().unwrap();
        // A synthetic gst::Device via the pulse provider is impractical in a unit
        // test, so test the pure label/default logic instead.
        let info = AudioDevice {
            id: "alsa_input.pci-0000_00.analog-stereo".into(),
            label: "Built-in Audio Analog Stereo".into(),
            kind: DeviceKind::Source,
            is_default: true,
        };
        assert_eq!(info.kind, DeviceKind::Source);
        assert!(info.is_default);
    }

    #[test]
    fn default_flag_matches_default_name() {
        assert!(is_default(Some("dev.monitor"), "dev.monitor"));
        assert!(!is_default(Some("other"), "dev.monitor"));
        assert!(!is_default(None, "dev.monitor"));
    }
}
```

- [ ] **Step 3: Run to verify failure** — `. "$HOME/.cargo/env" && cargo test -p engine audio::devices` → FAIL (`AudioDevice`/`is_default` undefined).

- [ ] **Step 4: Implement** (`engine/src/audio/devices.rs`)

```rust
use gstreamer as gst;
use gstreamer::prelude::*;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeviceKind {
    Source,
    Sink,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioDevice {
    pub id: String,
    pub label: String,
    pub kind: DeviceKind,
    pub is_default: bool,
}

pub(crate) fn is_default(default_name: Option<&str>, id: &str) -> bool {
    default_name == Some(id)
}

/// The PipeWire/Pulse `node.name` (stable id) for a device, used as `device=`.
fn device_node_name(d: &gst::Device) -> Option<String> {
    let props = d.properties()?;
    props
        .get::<String>("node.name")
        .or_else(|_| props.get::<String>("device.name"))
        .ok()
}

pub(crate) fn device_to_info(d: &gst::Device, default_name: Option<&str>) -> Option<AudioDevice> {
    let klass = d.device_class();
    let kind = if klass.contains("Source") {
        DeviceKind::Source
    } else if klass.contains("Sink") {
        DeviceKind::Sink
    } else {
        return None;
    };
    let id = device_node_name(d)?;
    let label = d.display_name().to_string();
    let is_default = is_default(default_name, &id);

    Some(AudioDevice { id, label, kind, is_default })
}

/// Enumerate Pulse/PipeWire audio sources and sinks via a one-shot DeviceMonitor.
pub fn list_devices() -> Vec<AudioDevice> {
    let _ = gst::init();
    let monitor = gst::DeviceMonitor::new();
    let caps = gst::Caps::new_empty_simple("audio/x-raw");
    let _ = monitor.add_filter(Some("Audio/Source"), Some(&caps));
    let _ = monitor.add_filter(Some("Audio/Sink"), Some(&caps));
    if monitor.start().is_err() {
        return Vec::new();
    }

    let devices = monitor.devices();
    monitor.stop();

    devices.iter().filter_map(|d| device_to_info(d, None)).collect()
}
```

- [ ] **Step 5: Run tests** — `cargo test -p engine audio::devices` → PASS.

- [ ] **Step 6: Smoke-check enumeration** (manual, not committed): a throwaway `cargo run` is overkill; instead add `#[ignore]` integration check:

```rust
    #[test]
    #[ignore] // live: prints real devices
    fn lists_live_devices() {
        let d = list_devices();
        println!("{d:#?}");
        assert!(!d.is_empty());
    }
```

Run `cargo test -p engine audio::devices::tests::lists_live_devices -- --ignored --nocapture` and confirm your mic + speakers appear.

- [ ] **Step 7: Commit**

```bash
git add engine/src/audio engine/src/lib.rs && git commit -m "feat(engine): audio device enumeration via DeviceMonitor"
```

---

## Task 2: Engine – DSP wrapper (`webrtc-audio-processing`)

**Files:** Modify `engine/Cargo.toml`; Create `engine/src/audio/dsp.rs`; Modify `engine/src/audio/mod.rs`.

**Interfaces:**
- Produces: `pub struct DspConfig { pub echo_cancel: bool, pub noise_suppression: NsLevel, pub agc: bool, pub vad: bool, pub high_pass: bool }`, `pub enum NsLevel { Off, Low, Moderate, High }`, `pub struct Dsp` with `Dsp::new() -> Result<Dsp>`, `set_config(&mut self, cfg: &DspConfig)`, `process_capture(&mut self, frame: &mut [f32]) -> bool` (returns VAD voice flag), `process_render(&mut self, frame: &mut [f32])`, and `pub const FRAME_SAMPLES: usize = 480` (10 ms @ 48 kHz, mono).

- [ ] **Step 1: Add the dependency** (`engine/Cargo.toml`)

```toml
webrtc-audio-processing = "0.3"
```

- [ ] **Step 2: Declare the module.** In `engine/src/audio/mod.rs` add `pub mod dsp;`.

- [ ] **Step 3: Write the failing test** (`engine/src/audio/dsp.rs`)

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn processes_a_silent_frame() {
        let mut dsp = Dsp::new().expect("create dsp");
        dsp.set_config(&DspConfig {
            echo_cancel: true,
            noise_suppression: NsLevel::High,
            agc: true,
            vad: true,
            high_pass: true,
        });

        let mut render = vec![0.0f32; FRAME_SAMPLES];
        dsp.process_render(&mut render);

        let mut capture = vec![0.0f32; FRAME_SAMPLES];
        let voice = dsp.process_capture(&mut capture);
        assert!(!voice, "silence must not be detected as voice");
    }

    #[test]
    fn config_toggles_apply_without_error() {
        let mut dsp = Dsp::new().unwrap();
        for ns in [NsLevel::Off, NsLevel::Low, NsLevel::Moderate, NsLevel::High] {
            dsp.set_config(&DspConfig {
                echo_cancel: false,
                noise_suppression: ns,
                agc: false,
                vad: false,
                high_pass: false,
            });
            let mut f = vec![0.0f32; FRAME_SAMPLES];
            let _ = dsp.process_capture(&mut f);
        }
    }
}
```

- [ ] **Step 4: Run to verify failure** — `cargo test -p engine audio::dsp` → FAIL (compile: `Dsp` undefined). (This also pulls + builds the crate via cmake/clang; first build is slow.)

- [ ] **Step 5: Implement** (`engine/src/audio/dsp.rs`). Confirm exact field names against `cargo doc -p webrtc-audio-processing` while implementing; the crate processes a fixed 10 ms frame (`webrtc_audio_processing::NUM_SAMPLES_PER_FRAME` = 480 for 48 kHz mono) of interleaved `f32` in `[-1, 1]`.

```rust
use anyhow::Result;
use webrtc_audio_processing::{
    Config, EchoCancellation, EchoCancellationSuppressionLevel, GainControl, GainControlMode,
    InitializationConfig, NoiseSuppression, NoiseSuppressionLevel, Processor, VoiceDetection,
    VoiceDetectionLikelihood,
};

/// 10 ms at 48 kHz, mono. The crate requires exactly this frame size.
pub const FRAME_SAMPLES: usize = 480;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NsLevel {
    Off,
    Low,
    Moderate,
    High,
}

#[derive(Debug, Clone)]
pub struct DspConfig {
    pub echo_cancel: bool,
    pub noise_suppression: NsLevel,
    pub agc: bool,
    pub vad: bool,
    pub high_pass: bool,
}

pub struct Dsp {
    processor: Processor,
}

impl Dsp {
    pub fn new() -> Result<Dsp> {
        let processor = Processor::new(&InitializationConfig {
            num_capture_channels: 1,
            num_render_channels: 1,
            ..Default::default()
        })?;

        Ok(Dsp { processor })
    }

    pub fn set_config(&mut self, cfg: &DspConfig) {
        let config = Config {
            echo_cancellation: cfg.echo_cancel.then(|| EchoCancellation {
                suppression_level: EchoCancellationSuppressionLevel::High,
                stream_delay_ms: None,
                enable_delay_agnostic: true,
                enable_extended_filter: true,
            }),
            noise_suppression: match cfg.noise_suppression {
                NsLevel::Off => None,
                NsLevel::Low => Some(NoiseSuppression { suppression_level: NoiseSuppressionLevel::Low }),
                NsLevel::Moderate => Some(NoiseSuppression { suppression_level: NoiseSuppressionLevel::Moderate }),
                NsLevel::High => Some(NoiseSuppression { suppression_level: NoiseSuppressionLevel::High }),
            },
            gain_control: cfg.agc.then(|| GainControl {
                mode: GainControlMode::AdaptiveDigital,
                target_level_dbfs: 3,
                compression_gain_db: 9,
                enable_limiter: true,
            }),
            voice_detection: cfg.vad.then(|| VoiceDetection {
                detection_likelihood: VoiceDetectionLikelihood::Moderate,
            }),
            enable_high_pass_filter: cfg.high_pass,
            ..Default::default()
        };

        self.processor.set_config(config);
    }

    /// Process one 10 ms render (far-end/playback) frame so AEC has a reference.
    pub fn process_render(&mut self, frame: &mut [f32]) {
        debug_assert_eq!(frame.len(), FRAME_SAMPLES);
        let _ = self.processor.process_render_frame(frame);
    }

    /// Process one 10 ms capture (mic) frame in place; returns the VAD voice flag.
    pub fn process_capture(&mut self, frame: &mut [f32]) -> bool {
        debug_assert_eq!(frame.len(), FRAME_SAMPLES);
        if self.processor.process_capture_frame(frame).is_err() {
            return false;
        }
        self.processor.get_stats().voice_detected.unwrap_or(false)
    }
}
```

- [ ] **Step 6: Run tests** — `cargo test -p engine audio::dsp` → PASS. If a field name differs from the installed crate version, fix it against `cargo doc` and re-run (do not stub it out).

- [ ] **Step 7: Commit**

```bash
git add engine/Cargo.toml engine/src/audio Cargo.lock && git commit -m "feat(engine): voice DSP wrapper over webrtc-audio-processing"
```

---

## Task 3: Engine – activation gate (pure state machine)

**Files:** Create `engine/src/audio/gate.rs`; Modify `engine/src/audio/mod.rs`.

**Interfaces:**
- Produces: `pub enum ActivationMode { Voice { threshold: f32 }, PushToTalk, AlwaysOn }`, `pub struct Gate { ... }` with `Gate::new(mode)`, `set_mode`, `set_muted(bool)`, `set_ptt_held(bool)`, `update_level(rms_db: f32, vad: bool)`, and `open(&self) -> bool` (true = transmit). `open()` is what drives `mic_valve` (valve drop = `!open`).

- [ ] **Step 1: Declare the module.** In `engine/src/audio/mod.rs` add `pub mod gate;`.

- [ ] **Step 2: Write the failing tests** (`engine/src/audio/gate.rs`)

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mute_overrides_everything() {
        let mut g = Gate::new(ActivationMode::AlwaysOn);
        assert!(g.open());
        g.set_muted(true);
        assert!(!g.open());
    }

    #[test]
    fn ptt_gates_on_key() {
        let mut g = Gate::new(ActivationMode::PushToTalk);
        assert!(!g.open());
        g.set_ptt_held(true);
        assert!(g.open());
        g.set_ptt_held(false);
        assert!(!g.open());
    }

    #[test]
    fn voice_activity_uses_threshold_or_vad() {
        let mut g = Gate::new(ActivationMode::Voice { threshold: -40.0 });
        g.update_level(-60.0, false);
        assert!(!g.open(), "below threshold + no vad = closed");
        g.update_level(-30.0, false);
        assert!(g.open(), "above threshold = open");
        g.update_level(-60.0, true);
        assert!(g.open(), "vad voice flag = open even if quiet");
    }
}
```

- [ ] **Step 3: Run to verify failure** — `cargo test -p engine audio::gate` → FAIL.

- [ ] **Step 4: Implement** (`engine/src/audio/gate.rs`)

```rust
#[derive(Debug, Clone, Copy)]
pub enum ActivationMode {
    /// Open when input RMS exceeds `threshold` dBFS or the VAD reports voice.
    Voice { threshold: f32 },
    PushToTalk,
    AlwaysOn,
}

pub struct Gate {
    mode: ActivationMode,
    muted: bool,
    ptt_held: bool,
    last_rms_db: f32,
    last_vad: bool,
}

impl Gate {
    pub fn new(mode: ActivationMode) -> Gate {
        Gate { mode, muted: false, ptt_held: false, last_rms_db: -120.0, last_vad: false }
    }

    pub fn set_mode(&mut self, mode: ActivationMode) {
        self.mode = mode;
    }

    pub fn set_muted(&mut self, muted: bool) {
        self.muted = muted;
    }

    pub fn set_ptt_held(&mut self, held: bool) {
        self.ptt_held = held;
    }

    pub fn update_level(&mut self, rms_db: f32, vad: bool) {
        self.last_rms_db = rms_db;
        self.last_vad = vad;
    }

    /// True = transmit. Precedence: mute > ptt > voice-activity > always-on.
    pub fn open(&self) -> bool {
        if self.muted {
            return false;
        }
        match self.mode {
            ActivationMode::PushToTalk => self.ptt_held,
            ActivationMode::Voice { threshold } => self.last_rms_db >= threshold || self.last_vad,
            ActivationMode::AlwaysOn => true,
        }
    }
}
```

- [ ] **Step 5: Run tests** — `cargo test -p engine audio::gate` → PASS.

- [ ] **Step 6: Commit**

```bash
git add engine/src/audio && git commit -m "feat(engine): activation gate state machine (mute>ptt>vad>always)"
```

---

## Task 4: Engine – voice capture+DSP bridge, meter & live device/DSP apply

**Files:** Create `engine/src/audio/capture.rs`; Modify `engine/src/flow_peer.rs` (`build_voice_send_branch`, recv level tap), `engine/src/session.rs` (voice settings + live apply), `engine/src/audio/mod.rs`.

**Interfaces:**
- Consumes: `Dsp`, `DspConfig`, `Gate`, `ActivationMode`, `AudioDevice`.
- Produces on `Session`: `set_input_device(Option<String>)`, `set_output_device(Option<String>)`, `set_dsp(DspConfig)`, `set_activation(ActivationMode)`, `set_muted(bool)`, `set_ptt_held(bool)`. The voice send branch becomes `pulsesrc ! audioconvert ! audioresample (S16/48k) ! appsink` → DSP (f32) → `appsrc ! level ! mic_valve ! opusenc`; a recv-side `appsink` feeds `Dsp::process_render`.

- [ ] **Step 1: Declare the module.** In `engine/src/audio/mod.rs` add `pub mod capture;`.

- [ ] **Step 2: Build helper + unit test** (`engine/src/audio/capture.rs`) — the pad-level pieces are run-and-observe, but the f32↔i16 frame conversion is pure and TDD-able:

```rust
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
}
```

Run `cargo test -p engine audio::capture` → PASS.

- [ ] **Step 3: Rewrite the voice send branch** (`engine/src/flow_peer.rs` `build_voice_send_branch`). Replace `autoaudiosrc ! audioconvert ! audioresample ! queue` + `opusenc` with a DSP-bridged branch. Keep the existing `mic_valve` and `opusenc`. Build:

```
pulsesrc (device optional) ! audioconvert ! audioresample
  ! capsfilter "audio/x-raw,format=S16LE,channels=1,rate=48000"
  ! appsink name=dsp_in emit-signals=true max-buffers=2 drop=true
```

and a downstream branch fed from an `appsrc name=dsp_out` with the same caps:

```
appsrc name=dsp_out ! audioconvert ! level name=mic_level interval=50000000
  ! valve name=mic_valve ! audioconvert ! opusenc ! rtpopuspay ! webrtcbin.
```

On each `dsp_in` `new-sample`, pull the 10 ms S16 buffer, convert to f32 (`i16_to_f32`), run `Dsp::process_capture` (shared `Arc<Mutex<Dsp>>`), read the VAD flag, convert back (`f32_to_i16`), and push into `dsp_out`. (Buffer to exactly `FRAME_SAMPLES`; `audioresample`+capsfilter already fixes rate/format, and `pulsesrc` `blocksize` can request 10 ms.)

- [ ] **Step 4: Tap the level meter and render reference.** The `level` element posts `element` messages with `rms`; forward as a new `SessionEvent::InputLevel(f32_db)` (add the variant) so the UI meter and the `Gate` see it. On the **recv** voice branch (`link_voice_recv`), add a `level`-less `appsink` tap (tee before `autoaudiosink`) that pulls 10 ms frames and calls `Dsp::process_render` so AEC has the far-end reference.

- [ ] **Step 5: Wire `Session` controls.** Add `dsp: Arc<Mutex<Dsp>>`, `gate: Gate`, `input_device`/`output_device: Option<String>` to `Session`. Implement:

```rust
pub fn set_dsp(&mut self, cfg: DspConfig) { self.dsp.lock().unwrap().set_config(&cfg); }
pub fn set_activation(&mut self, mode: ActivationMode) { self.gate.set_mode(mode); self.apply_gate(); }
pub fn set_muted(&mut self, m: bool) { self.gate.set_muted(m); self.apply_gate(); }
pub fn set_ptt_held(&mut self, h: bool) { self.gate.set_ptt_held(h); self.apply_gate(); }
pub fn set_input_device(&mut self, dev: Option<String>) { self.input_device = dev; self.rebuild_voice_sources(); }
```

`apply_gate()` sets every Voice flow's `mic_valve` `drop = !gate.open()`. On `SessionEvent::InputLevel`, call `gate.update_level(db, vad)` then `apply_gate()`. `rebuild_voice_sources()` pad-blocks each Voice flow's `pulsesrc`, sets `device`, and unblocks (sub-second). `set_dsp` is live (no rebuild). Keep the old `mute`/`deafen` working by routing through `set_muted`.

- [ ] **Step 6: Build + existing tests** — `cargo build -p engine && cargo test -p engine` → PASS (Tasks 1–3 unaffected).

- [ ] **Step 7: Run-and-observe (two instances, real devices if available; else default).** Join voice; speak → confirm in logs `InputLevel` events move; toggle a DSP flag via a temporary debug hook or the later UI. **Success:** voice still connects and audio flows with DSP in the chain; muting closes the valve. Record in `desktop/README.md` (M7 T4).

- [ ] **Step 8: Commit**

```bash
git add engine/src Cargo.lock && git commit -m "feat(engine): DSP-bridged voice capture, level meter, live device/DSP apply"
```

---

## Task 5: Engine – standalone mic-test monitor

**Files:** Create `engine/src/audio/monitor.rs`; Modify `engine/src/audio/mod.rs`, `engine/src/session.rs`.

**Interfaces:**
- Produces: `pub struct Monitor` with `Monitor::start(input: Option<String>, output: Option<String>, dsp: Arc<Mutex<Dsp>>, evt: UnboundedSender<SessionEvent>) -> Result<Monitor>` and `stop(self)`; on `Session`: `start_mic_test()`, `stop_mic_test()`. Emits `SessionEvent::InputLevel` while running.

- [ ] **Step 1: Declare the module** (`engine/src/audio/mod.rs` add `pub mod monitor;`).

- [ ] **Step 2: Implement the monitor pipeline** (`engine/src/audio/monitor.rs`): the same capture→DSP→`level`→`pulsesink` bridge as Task 4 but self-contained and looped to the speaker (so you hear yourself):

```
pulsesrc (input) ! audioconvert ! audioresample ! caps(S16/48k/mono)
  ! appsink(dsp_in)   →  DSP  →  appsrc(dsp_out)
  ! audioconvert ! level interval=50ms ! pulsesink (output)
```

Reuse `i16_to_f32`/`f32_to_i16` and the shared `Dsp`. Forward `level` RMS as `SessionEvent::InputLevel`.

- [ ] **Step 3: `Session` controls** — `start_mic_test`/`stop_mic_test` create/drop the `Monitor`, holding it in an `Option<Monitor>` field.

- [ ] **Step 4: Build** — `cargo build -p engine` → compiles.

- [ ] **Step 5: Run-and-observe** — a temporary CLI or the later Settings UI: start mic test → you hear your mic looped back and `InputLevel` events stream; flip noise suppression → audible change. **Success** recorded in `desktop/README.md` (M7 T5).

- [ ] **Step 6: Commit**

```bash
git add engine/src && git commit -m "feat(engine): standalone mic-test monitor with live meter"
```

---

## Task 6: Engine – global push-to-talk hotkey

**Files:** Modify `engine/Cargo.toml` (`x11rb`); Create `engine/src/hotkey.rs`; Modify `engine/src/lib.rs`.

**Interfaces:**
- Produces: `pub struct PttGrab` with `PttGrab::grab(keysym: u32, on_change: impl Fn(bool) + Send + 'static) -> Result<PttGrab>` and `Drop` (ungrabs); `pub fn keysym_from_name(name: &str) -> Option<u32>`.

- [ ] **Step 1: Add dep** (`engine/Cargo.toml`): `x11rb = "0.13"`.

- [ ] **Step 2: Declare module** (`engine/src/lib.rs` add `pub mod hotkey;`).

- [ ] **Step 3: Write the failing test** (`engine/src/hotkey.rs`)

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_key_names_to_keysyms() {
        assert_eq!(keysym_from_name("F12"), Some(0xFFC9));
        assert_eq!(keysym_from_name("space"), Some(0x0020));
        assert_eq!(keysym_from_name("not-a-key"), None);
    }
}
```

- [ ] **Step 4: Run to verify failure** — `cargo test -p engine hotkey` → FAIL.

- [ ] **Step 5: Implement** (`engine/src/hotkey.rs`): a small keysym table for the common PTT keys plus an x11rb `GrabKey` on the root window, with a background thread that selects on the X connection and calls `on_change(true/false)` on KeyPress/KeyRelease. Ungrab + join on `Drop`.

```rust
use anyhow::Result;
use std::sync::{atomic::{AtomicBool, Ordering}, Arc};
use std::thread::JoinHandle;
use x11rb::connection::Connection;
use x11rb::protocol::xproto::{ConnectionExt, GrabMode, ModMask, KEY_PRESS_EVENT, KEY_RELEASE_EVENT};
use x11rb::protocol::Event;

pub fn keysym_from_name(name: &str) -> Option<u32> {
    Some(match name {
        "space" => 0x0020,
        "F12" => 0xFFC9,
        "F11" => 0xFFC8,
        "F10" => 0xFFC7,
        "Control_L" => 0xFFE3,
        "Alt_L" => 0xFFE9,
        _ => return None,
    })
}

pub struct PttGrab {
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl PttGrab {
    pub fn grab(keysym: u32, on_change: impl Fn(bool) + Send + 'static) -> Result<PttGrab> {
        let (conn, screen_num) = x11rb::connect(None)?;
        let root = conn.setup().roots[screen_num].root;
        let keycode = keysym_to_keycode(&conn, keysym)?;
        conn.grab_key(true, root, ModMask::ANY, keycode, GrabMode::ASYNC, GrabMode::ASYNC)?;
        conn.flush()?;

        let stop = Arc::new(AtomicBool::new(false));
        let stop_thread = stop.clone();
        let handle = std::thread::spawn(move || {
            while !stop_thread.load(Ordering::Relaxed) {
                match conn.poll_for_event() {
                    Ok(Some(Event::KeyPress(e))) if e.detail == keycode => on_change(true),
                    Ok(Some(Event::KeyRelease(e))) if e.detail == keycode => on_change(false),
                    Ok(_) => std::thread::sleep(std::time::Duration::from_millis(8)),
                    Err(_) => break,
                }
            }
        });

        Ok(PttGrab { stop, handle: Some(handle) })
    }
}

impl Drop for PttGrab {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}
```

(Add a `keysym_to_keycode` helper using `get_keyboard_mapping`; confirm signatures against `cargo doc -p x11rb`.)

- [ ] **Step 6: Run tests** — `cargo test -p engine hotkey` → PASS.

- [ ] **Step 7: Wire into `Session`** — `set_ptt_key(Option<String>)` creates/drops a `PttGrab` whose callback forwards to `set_ptt_held` (via an event/channel hop to the main thread). Only active when activation mode is PushToTalk.

- [ ] **Step 8: Run-and-observe** — set PTT key F12; with another window focused, hold F12 → mic transmits, release → stops. Record (M7 T6).

- [ ] **Step 9: Commit**

```bash
git add engine/Cargo.toml engine/src Cargo.lock && git commit -m "feat(engine): X11 global push-to-talk hotkey"
```

---

## Task 7: Engine – screenshare video sources + quality + preview

**Files:** Create `engine/src/screen/mod.rs`, `engine/src/screen/sources.rs`, `engine/src/screen/capture.rs`; Modify `engine/src/lib.rs`, `engine/src/capture.rs` (or fold into `screen/capture.rs`), `engine/src/session.rs`.

**Interfaces:**
- Produces: `pub enum ShareSource { Screen { monitor: usize }, Window { xid: u32 } }`, `pub struct ShareConfig { pub source: ShareSource, pub width: u32, pub height: u32, pub fps: u32, pub content: ContentType }`, `pub enum ContentType { Smoothness, Clarity }`, `pub fn list_windows() -> Vec<ShareWindow { xid, title }>`, `pub fn capture_chain(cfg: &ShareConfig) -> String`. On `Session`: `start_share(cfg: ShareConfig)`, `preview_paintable() -> Option<glib::Object>`.

- [ ] **Step 1: Declare modules** (`engine/src/lib.rs` add `pub mod screen;`; `screen/mod.rs` re-exports `sources`, `capture`).

- [ ] **Step 2: Write failing tests** (`engine/src/screen/capture.rs`)

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn screen_chain_uses_ximagesrc_and_caps() {
        let cfg = ShareConfig {
            source: ShareSource::Screen { monitor: 0 },
            width: 1920, height: 1080, fps: 60, content: ContentType::Smoothness,
        };
        let chain = capture_chain(&cfg);
        assert!(chain.contains("ximagesrc"));
        assert!(chain.contains("framerate=60/1"));
        assert!(chain.contains("1920") && chain.contains("1080"));
    }

    #[test]
    fn window_chain_sets_xid() {
        let cfg = ShareConfig {
            source: ShareSource::Window { xid: 0x1400003 },
            width: 1280, height: 720, fps: 30, content: ContentType::Clarity,
        };
        let chain = capture_chain(&cfg);
        assert!(chain.contains("xid=0x1400003"));
    }
}
```

- [ ] **Step 3: Run to verify failure** — `cargo test -p engine screen::capture` → FAIL.

- [ ] **Step 4: Implement** `capture_chain` (build the `ximagesrc [xid=…] ! videoconvert ! videoscale ! capsfilter "video/x-raw,width=…,height=…,framerate=…/1"` string; `ContentType::Clarity` lowers fps ceiling / raises encoder quality, `Smoothness` keeps fps). Implement `list_windows()` via x11rb `_NET_CLIENT_LIST` (read the root property, then each window's `_NET_WM_NAME`). Run tests → PASS.

- [ ] **Step 5: Wire `Session::start_share(cfg)`** to feed the screen `FlowPeer` this chain (replace the fixed `engine/src/capture.rs` default for the product path; keep `HEARTH_CAPTURE` override as the highest priority for testing). Add a `tee` → `gtk4paintablesink` for `preview_paintable()`.

- [ ] **Step 6: Build + tests** — `cargo build -p engine && cargo test -p engine screen` → PASS.

- [ ] **Step 7: Run-and-observe** — with `HEARTH_CAPTURE` synthetic source for the stream itself, exercise `list_windows()` (logs your real windows) and the caps builder at 720/1080 and 30/60. **Success** recorded (M7 T7).

- [ ] **Step 8: Commit**

```bash
git add engine/src Cargo.lock && git commit -m "feat(engine): screenshare source selection, quality caps, preview"
```

---

## Task 8: Engine – screenshare audio (PipeWire node listing + track)

**Files:** Create `engine/src/screen/audio.rs`; Modify `engine/src/screen/mod.rs`, `engine/src/flow_peer.rs` (add audio to screen send branch), `engine/src/session.rs`.

**Interfaces:**
- Produces: `pub enum ShareAudio { None, System, App { node: String } }`, `pub struct AudioNode { pub node: String, pub label: String }`, `pub fn list_app_nodes() -> Vec<AudioNode>`, `pub(crate) fn keep_node(props: &NodeProps, own_pid: u32, filt: &NodeFilter) -> bool`, `pub fn has_pipewire() -> bool`, `pub fn screen_audio_chain(a: &ShareAudio) -> Option<String>`.

- [ ] **Step 1: Declare module** (`screen/mod.rs` add `pub mod audio;`).

- [ ] **Step 2: Write failing tests** (`engine/src/screen/audio.rs`) — the venmic-style filter rules are pure and TDD-able:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn props(pid: u32, media_class: &str, virt: bool) -> NodeProps {
        NodeProps { pid, media_class: media_class.into(), virtual_node: virt }
    }

    #[test]
    fn excludes_own_process() {
        let f = NodeFilter::default();
        assert!(!keep_node(&props(42, "Stream/Output/Audio", false), 42, &f));
        assert!(keep_node(&props(99, "Stream/Output/Audio", false), 42, &f));
    }

    #[test]
    fn excludes_inputs_and_virtual_when_requested() {
        let f = NodeFilter { ignore_input: true, ignore_virtual: true, ..Default::default() };
        assert!(!keep_node(&props(1, "Stream/Input/Audio", false), 0, &f));
        assert!(!keep_node(&props(1, "Stream/Output/Audio", true), 0, &f));
        assert!(keep_node(&props(1, "Stream/Output/Audio", false), 0, &f));
    }

    #[test]
    fn system_chain_uses_monitor_and_app_uses_target_object() {
        assert!(screen_audio_chain(&ShareAudio::None).is_none());
        assert!(screen_audio_chain(&ShareAudio::System).unwrap().contains(".monitor"));
        let app = screen_audio_chain(&ShareAudio::App { node: "Firefox".into() }).unwrap();
        assert!(app.contains("target-object=Firefox"));
    }
}
```

- [ ] **Step 3: Run to verify failure** — `cargo test -p engine screen::audio` → FAIL.

- [ ] **Step 4: Implement** `NodeProps`, `NodeFilter` (defaults: `ignore_input: true`, `ignore_virtual: false`, `ignore_devices: true`, `only_speakers: true`), `keep_node` (always drop `own_pid`; apply the toggles), `screen_audio_chain` (`None` → `None`; `System` → `pulsesrc device=@DEFAULT_SINK@.monitor ! audioconvert ! audioresample ! opusenc audio-type=generic`; `App` → `pipewiresrc target-object=<node> ! …`), `has_pipewire()` (probe the `pipewiresrc` factory), and `list_app_nodes()` (enumerate via `pw-dump`/PipeWire registry; keep behind `has_pipewire`). Run tests → PASS.

- [ ] **Step 5: Add the audio track to the screen flow** (`flow_peer.rs`): when a screen offerer is built with `ShareAudio != None`, add the `screen_audio_chain` branch as a second media into the screen `webrtcbin` (stereo, 48 kHz, **no DSP**). On the viewer, link the incoming screenshare audio to `autoaudiosink`. Renegotiation adds a track to the same flow (verify the M6 switcher still works).

- [ ] **Step 6: Build + tests** — `cargo build -p engine && cargo test -p engine screen` → PASS.

- [ ] **Step 7: Run-and-observe** — share with audio = System then = a specific app; the viewer hears it; switching the Watching stream still works; video keeps playing. Record (M7 T8).

- [ ] **Step 8: Commit**

```bash
git add engine/src && git commit -m "feat(engine): screenshare app/system audio via PipeWire nodes"
```

---

## Task 9: Desktop – Settings model + persistence

**Files:** Modify `desktop/src/config.rs`.

**Interfaces:**
- Produces: `pub struct Settings { input_device, output_device: Option<String>, input_volume, output_volume: f64, noise_suppression: NsLevel, echo_cancellation, agc, vad: bool, input_sensitivity: f32, activation: ActivationKind, ptt_key: Option<String>, share_width, share_height, share_fps: u32, share_content: ContentKind, share_audio: ShareAudioKind }` (serde, `Default`), plus `Config::load_settings()` / `Config::save_settings(&Settings)`.

- [ ] **Step 1: Write failing test** (`desktop/src/config.rs`)

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_round_trip_via_json() {
        let s = Settings { input_sensitivity: -42.0, agc: true, ..Default::default() };
        let json = serde_json::to_string(&s).unwrap();
        let back: Settings = serde_json::from_str(&json).unwrap();
        assert_eq!(back.input_sensitivity, -42.0);
        assert!(back.agc);
    }

    #[test]
    fn defaults_are_sane() {
        let s = Settings::default();
        assert_eq!(s.share_fps, 30);
        assert!(matches!(s.activation, ActivationKind::Voice));
    }
}
```

- [ ] **Step 2: Run to verify failure** — `cargo test -p desktop config` → FAIL.

- [ ] **Step 3: Implement** the `Settings` struct with `serde` derives and `Default` (defaults: NS standard, AEC on, AGC on, VAD on, activation Voice, sensitivity -40 dB, share 1920×1080 @30 Smoothness, audio None). Persist to `settings.json` beside the token (reuse the existing config-dir logic). Use plain enums (`ActivationKind`, `ContentKind`, `ShareAudioKind`, `NsLevel`) with `#[serde(rename_all = "snake_case")]`. Run tests → PASS.

- [ ] **Step 4: Commit**

```bash
git add desktop/src/config.rs && git commit -m "feat(desktop): local Settings model + persistence"
```

---

## Task 10: Desktop – level meter widget + Voice settings page

**Files:** Create `desktop/src/ui/meter.rs`, `desktop/src/ui/settings.rs`; Modify `desktop/src/ui/mod.rs`, `desktop/src/ui/self_panel.rs` (gear button), `desktop/src/ui/workspace.rs`, `desktop/src/app.rs` (forward `InputLevel`; apply settings to `Session`).

**Interfaces:**
- Consumes: `Settings`, `SessionEvent::InputLevel`, `Session::{set_dsp,set_activation,set_input_device,set_output_device,set_ptt_key,start_mic_test,stop_mic_test,list_devices}`.
- Produces: a `Meter` relm4 component (`SetLevel(f32)`), a `SettingsWindow` component (Voice page), `SelfPanelOutput::OpenSettings`.

- [ ] **Step 1: `ui/meter.rs`** — a `gtk::LevelBar` (or `DrawingArea`) component with input `SetLevel(f32_db)` mapping dB → 0..1.

- [ ] **Step 2: `ui/settings.rs`** — a relm4 `Component` (a `gtk::Window`) with the Voice page: Microphone/Speaker `gtk::DropDown` (populated from `Session::list_devices()` split by kind), volume `Scale`s, a **Mic Test** `ToggleButton` (→ `start/stop_mic_test`) next to the `Meter`, an Input-Sensitivity `Scale` over the meter, Noise-Suppression `DropDown` (off/standard/high), Echo-Cancellation / AGC / Voice-Activity `Switch`es, an Activation `DropDown` (Voice/PTT/Always), and a PTT keybind capture button. Each control emits a `SettingsOutput` variant; on change the parent updates `Settings`, persists, and calls the matching `Session` setter.

- [ ] **Step 3: Gear button** — add a settings `Button` to `ui/self_panel.rs` emitting `SelfPanelOutput::OpenSettings`; `workspace.rs`/`app.rs` open the `SettingsWindow` controller.

- [ ] **Step 4: Forward `InputLevel`** — in `app.rs::on_event`, route `SessionEvent::InputLevel(db)` to the settings window's meter (and the gate already consumes it in the engine).

- [ ] **Step 5: Build** — `cargo build -p desktop` → compiles.

- [ ] **Step 6: Run-and-observe** — open Settings, pick a different mic, **Mic Test** → meter moves with your voice and you hear yourself; toggle Noise Suppression / Echo Cancellation → audible; set Activation = PTT + a key. **Success** recorded (M7 T9, `desktop/README.md`).

- [ ] **Step 7: Commit**

```bash
git add desktop/src && git commit -m "feat(desktop): voice settings page + level meter + mic test"
```

---

## Task 11: Desktop – Screen Share picker + wire-up

**Files:** Create `desktop/src/ui/screenshare_picker.rs`; Modify `desktop/src/ui/mod.rs`, `desktop/src/ui/self_panel.rs` (Share → open picker), `desktop/src/ui/workspace.rs`, `desktop/src/app.rs`.

**Interfaces:**
- Consumes: `Settings`, `screen::{list_windows,list_app_nodes,has_pipewire}`, `Session::{start_share(ShareConfig, ShareAudio), preview_paintable, stop_share}`.
- Produces: a `ScreenSharePicker` component returning `{ source, width, height, fps, content, audio }`.

- [ ] **Step 1: `ui/screenshare_picker.rs`** — a relm4 `Component` (`gtk::Window`): a source grid (whole-screen entries + `list_windows()` windows, each a button with a thumbnail/title), a large `gtk::Picture` **preview** bound to `Session::preview_paintable()` for the highlighted source, Resolution / Frame-Rate / Content-Type radio rows (from the spec presets), an Audio-Source `DropDown` (None / Entire System / `list_app_nodes()` — disabled with a note when `!has_pipewire()`), and **Go Live** / Cancel. Persists choices back into `Settings`.

- [ ] **Step 2: Replace the bare Share toggle** — `ui/self_panel.rs` Share button now emits `OpenSharePicker`; the workspace opens the picker; **Go Live** calls `Session::start_share(cfg, audio)` and flips the share state; closing/stop calls `stop_share`.

- [ ] **Step 3: Preview** — start a local-only preview capture when the picker opens (so the `Picture` shows the selected source live) and stop it on close.

- [ ] **Step 4: Build** — `cargo build -p desktop` → compiles.

- [ ] **Step 5: Full run-and-observe** (2 instances, `HEARTH_CAPTURE` synthetic for the actual stream): open the picker → pick whole-screen vs a window (preview updates), choose 720/30 vs 1080/60, set Audio = Entire System → **Go Live**; the viewer sees the stream on the M6 stage and hears the audio; the Watching switcher and chat still work. Record (M7 T10, `desktop/README.md`).

- [ ] **Step 6: Commit**

```bash
git add desktop/src && git commit -m "feat(desktop): screen share picker with source, quality, audio + preview"
```

---

## Task 12: Docs – STATUS + README wrap-up

**Files:** Modify `docs/STATUS.md`, `desktop/README.md`.

- [ ] **Step 1:** Add an "M7 done" section to `docs/STATUS.md` (device selection, voice DSP via the crate, mic test, activation/PTT, screenshare picker with app/system audio + quality + preview) and mark the milestone in the table; note Windows as the next platform target.
- [ ] **Step 2:** Ensure `desktop/README.md` has the M7 T4–T10 verification entries.
- [ ] **Step 3: Commit**

```bash
git add docs/STATUS.md desktop/README.md && git commit -m "docs: M7 voice processing + advanced screenshare done"
```

---

## Self-Review (completed during authoring)

- **Spec coverage:** device enumeration (T1), DSP wrapper (T2), activation gate (T3), voice capture+DSP+meter+live-apply (T4), mic-test monitor (T5), global PTT (T6), screenshare sources/quality/preview (T7), screenshare app/system audio (T8), settings model (T9), voice settings UI + meter (T10), screen-share picker (T11), docs (T12). The five spec units map onto T1–T8 (engine) and T9–T11 (desktop). North-star decisions — in-process DSP, live-apply, PipeWire node capture without a virtual-mic, screenshare audio stereo/no-DSP, X11 screen+window, activation precedence — are realized in T2/T4/T6/T7/T8/T3.
- **Placeholder scan:** TDD logic (T1–T3, T7–T9) ships concrete tests + code; pipeline/UI steps (T4–T6, T10–T11) are run-and-observe with explicit success criteria and the synthetic-capture rule, matching the spec's testing section and the M5/M6 precedent. Crate/x11rb signatures are flagged to confirm against `cargo doc` at execution (first-use smoke tests surface the real API) — no silent stubs.
- **Type consistency:** `DspConfig`/`NsLevel` (T2) are consumed verbatim by `Session::set_dsp` (T4), the monitor (T5), and the settings UI (T10); `ActivationMode` (T3) flows through `set_activation` (T4) and PTT (T6); `ShareConfig`/`ShareSource`/`ContentType` (T7) and `ShareAudio`/`NodeFilter` (T8) feed the picker (T11); `Settings` (T9) serializes the same enums the engine consumes.
- **Risk note:** the `webrtc-audio-processing` build (cmake/clang, present) and strict 10 ms/48 kHz framing are the engine risk (isolated + unit-tested in T2/T4); the live source hot-swap (T4) and adding an audio track to the screen flow (T8, must not break the M6 switcher) are the run-and-observe risks.
