# Voice DSP Profiles + RT-safety Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a cross-platform voice DSP profile layer (Custom/Headset/Speaker/Auto) with Linux output-device classification, an RT-scheduling warning, and a re-probe action — built above the existing per-platform DSP engines.

**Architecture:** Approach C — the DSP engines stay per-platform (Linux `webrtc-audio-processing`, Windows pure-Rust) behind the existing `audio::dsp`/`Session::set_dsp` seam. New pure logic (`profile`, `classify`, `rt`) lives in `engine/src/audio/`; the desktop computes the effective `DspConfig` from (profile, custom slot, classification) and feeds the existing `set_dsp`.

**Tech Stack:** Rust, GStreamer (`gst::DeviceMonitor` for device props), relm4/GTK4 desktop, serde for settings.

## Global Constraints

- Default behavior unchanged: profile defaults to **Custom**, and the existing flags default all-off (`NsLevel::Off`, `echo_cancellation=false`, `agc=false`, `vad=false`).
- Presets are constants and can never produce an invalid `DspConfig`.
- Classification never errors: any failure → `OutputKind::Unknown`. Windows classification is deferred (always `Unknown`).
- RT check is a warning only — no audio-path change.
- No new crate dependencies (RT probe reads `/proc/self/limits`, no `libc`).
- Engine tasks are TDD with unit tests; desktop UI tasks are run-and-observe (project convention), but their pure helpers are unit-tested.
- Commit message style: no `Co-Authored-By`/Claude attribution. Don't push.
- Build always via the workspace `.cargo/config.toml` (speex include flags already set); run tests with `cargo test -p engine`.

**Preset values (single source of truth, used everywhere):**
- `HEADSET_PRESET = DspConfig { echo_cancel: false, noise_suppression: NsLevel::Moderate, agc: true, vad: true, high_pass: true }`
- `SPEAKER_PRESET = DspConfig { echo_cancel: true,  noise_suppression: NsLevel::Moderate, agc: true, vad: true, high_pass: true }`

---

### Task 1: Profile core (enums, presets, resolver)

**Files:**
- Create: `engine/src/audio/profile.rs`
- Modify: `engine/src/audio/mod.rs` (add `pub mod profile;`)
- Test: in `engine/src/audio/profile.rs` (`#[cfg(test)] mod tests`)

**Interfaces:**
- Consumes: `crate::audio::dsp::{DspConfig, NsLevel}`.
- Produces:
  - `pub enum VoiceProfile { Custom, Headset, Speaker, Auto }` (derives `Debug, Clone, Copy, PartialEq, Eq`)
  - `pub enum OutputKind { Headphones, Speakers, Unknown }` (same derives)
  - `pub fn headset_preset() -> DspConfig`
  - `pub fn speaker_preset() -> DspConfig`
  - `pub fn preset_for(kind: OutputKind) -> DspConfig`
  - `pub fn effective(profile: VoiceProfile, custom: &DspConfig, output: OutputKind) -> DspConfig`

- [ ] **Step 1: Write the failing tests**

In `engine/src/audio/profile.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::dsp::NsLevel;

    #[test]
    fn presets_differ_only_on_aec() {
        assert!(!headset_preset().echo_cancel);
        assert!(speaker_preset().echo_cancel);
        assert!(headset_preset().agc && speaker_preset().agc);
        assert_eq!(headset_preset().noise_suppression, NsLevel::Moderate);
    }

    #[test]
    fn custom_passes_through_untouched() {
        let custom = DspConfig {
            echo_cancel: false,
            noise_suppression: NsLevel::Off,
            agc: true,
            vad: false,
            high_pass: false,
        };
        let got = effective(VoiceProfile::Custom, &custom, OutputKind::Speakers);
        assert_eq!(got.agc, true);
        assert_eq!(got.echo_cancel, false); // ignores the Speakers classification
    }

    #[test]
    fn auto_resolves_by_output_kind() {
        let custom = headset_preset();
        assert!(!effective(VoiceProfile::Auto, &custom, OutputKind::Headphones).echo_cancel);
        assert!(effective(VoiceProfile::Auto, &custom, OutputKind::Speakers).echo_cancel);
        // Unknown is the safe low-latency default = Headset (AEC off).
        assert!(!effective(VoiceProfile::Auto, &custom, OutputKind::Unknown).echo_cancel);
    }

    #[test]
    fn explicit_presets_ignore_classification() {
        let custom = DspConfig {
            echo_cancel: false, noise_suppression: NsLevel::Off,
            agc: false, vad: false, high_pass: false,
        };
        assert!(!effective(VoiceProfile::Headset, &custom, OutputKind::Speakers).echo_cancel);
        assert!(effective(VoiceProfile::Speaker, &custom, OutputKind::Headphones).echo_cancel);
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p engine profile:: 2>&1 | tail -20`
Expected: FAIL — `cannot find ... VoiceProfile / effective` (module not declared).

- [ ] **Step 3: Write the implementation**

At the top of `engine/src/audio/profile.rs`:

```rust
use crate::audio::dsp::{DspConfig, NsLevel};

/// User-facing voice processing profile. `Custom` is the user's hand-tuned
/// config; the presets are read-only views; `Auto` resolves from the output
/// device's form factor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VoiceProfile {
    Custom,
    Headset,
    Speaker,
    Auto,
}

/// Acoustic class of the active output device, used to resolve `Auto`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputKind {
    Headphones,
    Speakers,
    Unknown,
}

/// Headset: no AEC (mic can't hear the headphones), NS/AGC/HPF on. Lowest latency.
pub fn headset_preset() -> DspConfig {
    DspConfig {
        echo_cancel: false,
        noise_suppression: NsLevel::Moderate,
        agc: true,
        vad: true,
        high_pass: true,
    }
}

/// Speaker: full processing including AEC for the open-air echo path.
pub fn speaker_preset() -> DspConfig {
    DspConfig {
        echo_cancel: true,
        noise_suppression: NsLevel::Moderate,
        agc: true,
        vad: true,
        high_pass: true,
    }
}

/// Resolve a classification to a preset. `Unknown` is the safe low-latency default.
pub fn preset_for(kind: OutputKind) -> DspConfig {
    match kind {
        OutputKind::Speakers => speaker_preset(),
        OutputKind::Headphones | OutputKind::Unknown => headset_preset(),
    }
}

/// The effective DSP config the engine should run for the given profile.
pub fn effective(profile: VoiceProfile, custom: &DspConfig, output: OutputKind) -> DspConfig {
    match profile {
        VoiceProfile::Custom => custom.clone(),
        VoiceProfile::Headset => headset_preset(),
        VoiceProfile::Speaker => speaker_preset(),
        VoiceProfile::Auto => preset_for(output),
    }
}
```

In `engine/src/audio/mod.rs`, add alongside the other `pub mod` lines:

```rust
pub mod profile;
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p engine profile:: 2>&1 | tail -20`
Expected: PASS — 4 tests.

- [ ] **Step 5: Commit**

```bash
git add engine/src/audio/profile.rs engine/src/audio/mod.rs
git commit -m "feat(voice): DSP profile core (Custom/Headset/Speaker/Auto presets + resolver)"
```

---

### Task 2: Output device classification (Linux)

**Files:**
- Create: `engine/src/audio/classify.rs`
- Modify: `engine/src/audio/mod.rs` (add `pub mod classify;`)
- Modify: `engine/src/audio/devices.rs` (no API change; `device_to_info` is already `pub(crate)` and reused)
- Test: in `engine/src/audio/classify.rs`

**Interfaces:**
- Consumes: `crate::audio::profile::OutputKind`, `crate::audio::devices::device_to_info`, `gstreamer`.
- Produces:
  - `pub fn classify_output(output_id: Option<&str>) -> OutputKind`
  - `pub(crate) fn kind_from(form_factor: Option<&str>, label: &str) -> OutputKind`

- [ ] **Step 1: Write the failing tests** (pure parser only — the live path is `#[ignore]`)

In `engine/src/audio/classify.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn form_factor_wins() {
        assert_eq!(kind_from(Some("headphone"), "whatever"), OutputKind::Headphones);
        assert_eq!(kind_from(Some("headset"), "x"), OutputKind::Headphones);
        assert_eq!(kind_from(Some("speaker"), "x"), OutputKind::Speakers);
        assert_eq!(kind_from(Some("internal"), "x"), OutputKind::Speakers);
    }

    #[test]
    fn label_fallback_when_no_form_factor() {
        assert_eq!(kind_from(None, "Logitech PRO X Headphones"), OutputKind::Headphones);
        assert_eq!(kind_from(None, "Built-in Speaker"), OutputKind::Speakers);
        assert_eq!(kind_from(None, "Generic USB Audio"), OutputKind::Unknown);
    }

    #[test]
    fn unknown_form_factor_falls_through_to_label() {
        assert_eq!(kind_from(Some("car"), "USB Headset"), OutputKind::Headphones);
        assert_eq!(kind_from(Some("car"), "Mystery Box"), OutputKind::Unknown);
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p engine classify:: 2>&1 | tail -20`
Expected: FAIL — module not found.

- [ ] **Step 3: Write the implementation**

```rust
use crate::audio::devices::device_to_info;
use crate::audio::profile::OutputKind;
use gstreamer as gst;
use gstreamer::prelude::*;

/// Classify a string against known form-factor / name hints.
pub(crate) fn kind_from(form_factor: Option<&str>, label: &str) -> OutputKind {
    let ff = form_factor.unwrap_or("").to_ascii_lowercase();
    if ff.contains("head") || ff.contains("hands-free") || ff.contains("earbud") {
        return OutputKind::Headphones;
    }
    if ff.contains("speaker") || ff == "internal" || ff.contains("hifi") || ff == "tv" {
        return OutputKind::Speakers;
    }

    let l = label.to_ascii_lowercase();
    if l.contains("headphone") || l.contains("headset") || l.contains("earbud") {
        return OutputKind::Headphones;
    }
    if l.contains("speaker") {
        return OutputKind::Speakers;
    }
    OutputKind::Unknown
}

/// Classify the active output device. Linux reads the sink's PipeWire/Pulse
/// `device.form_factor` and display name; Windows is deferred (always `Unknown`).
/// `output_id` is the saved device id (`None` = system default).
#[cfg(target_os = "windows")]
pub fn classify_output(_output_id: Option<&str>) -> OutputKind {
    // TODO: WASAPI PKEY_AudioEndpoint_FormFactor.
    OutputKind::Unknown
}

#[cfg(not(target_os = "windows"))]
pub fn classify_output(output_id: Option<&str>) -> OutputKind {
    let _ = gst::init();
    let monitor = gst::DeviceMonitor::new();
    let caps = gst::Caps::new_empty_simple("audio/x-raw");
    let _ = monitor.add_filter(Some("Audio/Sink"), Some(&caps));
    if monitor.start().is_err() {
        return OutputKind::Unknown;
    }
    let devices = monitor.devices();
    monitor.stop();

    for d in devices.iter() {
        let Some(info) = device_to_info(d, None) else { continue };
        let matches = match output_id {
            Some(id) => info.id == id,
            None => info.is_default,
        };
        if !matches {
            continue;
        }
        let form_factor = d
            .properties()
            .and_then(|p| p.get::<String>("device.form_factor").ok());
        return kind_from(form_factor.as_deref(), &d.display_name());
    }
    OutputKind::Unknown
}
```

Add to `engine/src/audio/mod.rs`:

```rust
pub mod classify;
```

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p engine classify:: 2>&1 | tail -20`
Expected: PASS — 3 tests. Also run `cargo build -p engine` to confirm the live path compiles.

- [ ] **Step 5: Commit**

```bash
git add engine/src/audio/classify.rs engine/src/audio/mod.rs
git commit -m "feat(voice): Linux output-device classification (form-factor + name)"
```

---

### Task 3: RT-scheduling probe + Warning event

**Files:**
- Create: `engine/src/audio/rt.rs`
- Modify: `engine/src/audio/mod.rs` (add `pub mod rt;`)
- Modify: `engine/src/session.rs` (add `Warning(String)` to `SessionEvent`; emit on startup)
- Test: in `engine/src/audio/rt.rs`

**Interfaces:**
- Produces:
  - `pub fn realtime_available() -> bool`
  - `pub(crate) fn parse_rtprio_limit(proc_limits: &str) -> Option<u64>`
  - New enum variant `SessionEvent::Warning(String)`.

- [ ] **Step 1: Write the failing tests**

In `engine/src/audio/rt.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
Limit                     Soft Limit           Hard Limit           Units
Max cpu time              unlimited            unlimited            seconds
Max realtime priority     95                   95
Max realtime timeout      unlimited            unlimited            us";

    const SAMPLE_ZERO: &str = "Max realtime priority     0                    0";

    #[test]
    fn parses_soft_rtprio() {
        assert_eq!(parse_rtprio_limit(SAMPLE), Some(95));
        assert_eq!(parse_rtprio_limit(SAMPLE_ZERO), Some(0));
        assert_eq!(parse_rtprio_limit("nothing here"), None);
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p engine rt:: 2>&1 | tail -20`
Expected: FAIL — module not found.

- [ ] **Step 3: Write the implementation**

`engine/src/audio/rt.rs`:

```rust
/// Parse the soft "Max realtime priority" limit out of `/proc/self/limits`.
pub(crate) fn parse_rtprio_limit(proc_limits: &str) -> Option<u64> {
    let line = proc_limits
        .lines()
        .find(|l| l.starts_with("Max realtime priority"))?;
    // Columns after the label: "<soft> <hard> [units]".
    line.split_whitespace().nth(3).and_then(|s| s.parse().ok())
}

/// Whether the audio path can obtain realtime scheduling. On Linux this means
/// the user is permitted a non-zero RT priority (PAM limits / rtkit); PipeWire
/// then runs RT. Windows/macOS assume MMCSS/equivalent for now.
#[cfg(target_os = "linux")]
pub fn realtime_available() -> bool {
    match std::fs::read_to_string("/proc/self/limits") {
        Ok(s) => parse_rtprio_limit(&s).map(|n| n > 0).unwrap_or(false),
        Err(_) => false,
    }
}

#[cfg(not(target_os = "linux"))]
pub fn realtime_available() -> bool {
    // TODO(Windows): confirm MMCSS "Pro Audio" registration.
    true
}
```

Add to `engine/src/audio/mod.rs`:

```rust
pub mod rt;
```

In `engine/src/session.rs`, add the variant to `SessionEvent` (after `Error(String)`):

```rust
    /// Non-fatal diagnostic surfaced to the UI/log (e.g. realtime scheduling
    /// unavailable, so latency may be unstable).
    Warning(String),
```

In `Session::new` (the constructor that has `evt_tx` in scope), just before it returns the constructed `Self`, emit the RT warning:

```rust
        if !crate::audio::rt::realtime_available() {
            let _ = evt_tx.send(SessionEvent::Warning(
                "audio not running with realtime priority; latency may be unstable".into(),
            ));
        }
```

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p engine rt:: 2>&1 | tail -20`
Expected: PASS — 1 test.
Run: `cargo build -p engine 2>&1 | tail -5`
Expected: builds (any `match` on `SessionEvent` in the desktop is handled in Task 5; engine itself must compile — if the engine has an exhaustive match on `SessionEvent`, add a `Warning` arm there in this task).

- [ ] **Step 5: Commit**

```bash
git add engine/src/audio/rt.rs engine/src/audio/mod.rs engine/src/session.rs
git commit -m "feat(voice): RT-scheduling probe + SessionEvent::Warning on startup"
```

---

### Task 4: Desktop settings — profile field + state transition

**Files:**
- Modify: `desktop/src/config.rs` (add `VoiceProfile` mirror enum, `Settings.profile`, transition helper)
- Test: in `desktop/src/config.rs` (`#[cfg(test)] mod tests`)

**Interfaces:**
- Consumes: `engine::audio::profile::{VoiceProfile as EngineProfile, OutputKind, effective}`, `engine::audio::dsp::{DspConfig, NsLevel as EngineNsLevel}`.
- Produces (in `desktop::config`):
  - `pub enum VoiceProfile { Custom, Headset, Speaker, Auto }` (serde, `#[default] Custom`)
  - `Settings.profile: VoiceProfile`
  - `pub fn settings_custom_dsp(s: &Settings) -> DspConfig` — maps the stored flags to an engine `DspConfig` (the custom slot).
  - `pub fn write_dsp(s: &mut Settings, d: &DspConfig)` — reverse map (for demote-on-edit + read-only display).
  - `pub fn demote_to_custom(s: &mut Settings, output: OutputKind)` — materialize the current effective into the flag fields and set `profile = Custom`.

- [ ] **Step 1: Write the failing tests**

Add to the `tests` module in `desktop/src/config.rs`:

```rust
#[test]
fn profile_defaults_to_custom_and_round_trips() {
    let s = Settings::default();
    assert!(matches!(s.profile, VoiceProfile::Custom));
    let json = serde_json::to_string(&s).unwrap();
    let back: Settings = serde_json::from_str(&json).unwrap();
    assert!(matches!(back.profile, VoiceProfile::Custom));
}

#[test]
fn old_settings_without_profile_load_as_custom() {
    // A settings blob saved before `profile` existed must still load.
    let json = r#"{"input_device":null,"output_device":null,"input_volume":1.0,
        "output_volume":1.0,"noise_suppression":"off","echo_cancellation":false,
        "agc":false,"vad":false,"input_sensitivity":-40.0,"activation":"voice",
        "ptt_key":null,"share_width":1920,"share_height":1080,"share_fps":30,
        "share_content":"smoothness","share_audio":"none","share_bitrate_kbps":6000}"#;
    let s: Settings = serde_json::from_str(json).unwrap();
    assert!(matches!(s.profile, VoiceProfile::Custom));
}

#[test]
fn demote_materializes_preset_then_becomes_custom() {
    // On Headset, the stored custom flags are all-off, but demoting must write
    // the Headset effective (AEC off, NS moderate, AGC on) into the flags.
    let mut s = Settings { profile: VoiceProfile::Headset, ..Default::default() };
    demote_to_custom(&mut s, OutputKind::Unknown);
    assert!(matches!(s.profile, VoiceProfile::Custom));
    assert!(s.agc);                 // from the Headset preset
    assert!(!s.echo_cancellation);  // Headset = AEC off
    assert!(matches!(s.noise_suppression, NsLevel::Moderate));
}

#[test]
fn selecting_preset_does_not_touch_stored_flags() {
    // Switching to a preset is a view; the custom slot (flags) stays as-is.
    let mut s = Settings { agc: true, ..Default::default() }; // custom = AGC only
    s.profile = VoiceProfile::Speaker;
    // No demote called (no edit) -> flags unchanged.
    assert!(s.agc);
    assert!(!s.echo_cancellation);
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p desktop config:: 2>&1 | tail -20`
Expected: FAIL — `VoiceProfile` / `demote_to_custom` not found.

- [ ] **Step 3: Write the implementation**

Add the enum near the other settings enums in `desktop/src/config.rs`:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum VoiceProfile {
    #[default]
    Custom,
    Headset,
    Speaker,
    Auto,
}
```

Add the field to `Settings` (serde default keeps old blobs loadable):

```rust
    #[serde(default)]
    pub profile: VoiceProfile,
```

Add `profile: VoiceProfile::Custom,` to the `Default for Settings` body.

Add the mapping helpers (put them after the `Settings` impl). Mirror the existing `NsLevel` mapping used in `desktop/src/app.rs`:

```rust
fn to_engine_ns(n: NsLevel) -> engine::audio::dsp::NsLevel {
    use engine::audio::dsp::NsLevel as E;
    match n {
        NsLevel::Off => E::Off,
        NsLevel::Low => E::Low,
        NsLevel::Moderate => E::Moderate,
        NsLevel::High => E::High,
    }
}

fn from_engine_ns(n: engine::audio::dsp::NsLevel) -> NsLevel {
    use engine::audio::dsp::NsLevel as E;
    match n {
        E::Off => NsLevel::Off,
        E::Low => NsLevel::Low,
        E::Moderate => NsLevel::Moderate,
        E::High => NsLevel::High,
    }
}

fn to_engine_profile(p: VoiceProfile) -> engine::audio::profile::VoiceProfile {
    use engine::audio::profile::VoiceProfile as E;
    match p {
        VoiceProfile::Custom => E::Custom,
        VoiceProfile::Headset => E::Headset,
        VoiceProfile::Speaker => E::Speaker,
        VoiceProfile::Auto => E::Auto,
    }
}

/// The user's custom slot, as an engine `DspConfig` (the stored flag fields).
pub fn settings_custom_dsp(s: &Settings) -> engine::audio::dsp::DspConfig {
    engine::audio::dsp::DspConfig {
        echo_cancel: s.echo_cancellation,
        noise_suppression: to_engine_ns(s.noise_suppression),
        agc: s.agc,
        vad: s.vad,
        high_pass: true,
    }
}

/// Write an engine `DspConfig` back into the flag fields (display + demote).
pub fn write_dsp(s: &mut Settings, d: &engine::audio::dsp::DspConfig) {
    s.echo_cancellation = d.echo_cancel;
    s.noise_suppression = from_engine_ns(d.noise_suppression);
    s.agc = d.agc;
    s.vad = d.vad;
}

/// The effective `DspConfig` for the current profile + classification.
pub fn effective_dsp(s: &Settings, output: engine::audio::profile::OutputKind)
    -> engine::audio::dsp::DspConfig
{
    engine::audio::profile::effective(to_engine_profile(s.profile), &settings_custom_dsp(s), output)
}

/// Apply the "editing a preset demotes to Custom" rule: materialize the current
/// effective config into the flag fields, then switch the profile to Custom. The
/// caller applies the user's single edit after this (or before, on the widget).
pub fn demote_to_custom(s: &mut Settings, output: engine::audio::profile::OutputKind) {
    let eff = effective_dsp(s, output);
    write_dsp(s, &eff);
    s.profile = VoiceProfile::Custom;
}
```

Ensure `engine` is a dependency of `desktop` (it already is — `desktop/src/app.rs` imports `engine::audio::dsp`). Add `use engine::audio::profile::OutputKind;` and `NsLevel` is already in scope in tests via `super::*`.

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p desktop config:: 2>&1 | tail -20`
Expected: PASS — 4 new tests plus the existing config tests.

- [ ] **Step 5: Commit**

```bash
git add desktop/src/config.rs
git commit -m "feat(voice): desktop profile state + effective/demote helpers"
```

---

### Task 5: Desktop wiring — apply effective config + Warning surface

**Files:**
- Modify: `desktop/src/app.rs` (compute effective in the settings-sync path; map `OutputKind` for Auto; handle `SessionEvent::Warning`)
- Test: run-and-observe (logic covered by Task 4 unit tests)

**Interfaces:**
- Consumes: `desktop::config::{effective_dsp, settings_custom_dsp}`, `engine::audio::classify::classify_output`, `engine::audio::profile::OutputKind`, `SessionEvent::Warning`.
- Produces: the live session's DSP now reflects the selected profile.

- [ ] **Step 1: Find the settings-sync site**

Run: `rg -n 'set_dsp|SessionEvent::Error|SessionEvent::' desktop/src/app.rs | head`
The `session.set_dsp(DspConfig { .. })` call (around `app.rs:553`) currently builds the config directly from `Settings`.

- [ ] **Step 2: Replace the direct build with the effective computation**

Replace the `session.set_dsp(DspConfig { ... })` block with a profile-aware version. For `Auto`, classify the current output; otherwise classification is irrelevant (pass `Unknown`):

```rust
        let output_kind = if matches!(s.profile, crate::config::VoiceProfile::Auto) {
            engine::audio::classify::classify_output(s.output_device.as_deref())
        } else {
            engine::audio::profile::OutputKind::Unknown
        };
        session.set_dsp(crate::config::effective_dsp(s, output_kind));
```

(Keep the existing `set_activation`, `set_input_device`, `set_output_device` calls that follow.)

- [ ] **Step 3: Handle the new Warning event**

Find where `SessionEvent::Error` is matched in `app.rs` and add a sibling arm. Match the existing error-handling style (e.g. logging / toast). Minimal:

```rust
            SessionEvent::Warning(msg) => {
                eprintln!("[hearth] warning: {msg}");
            }
```

- [ ] **Step 4: Build + run-and-observe**

Run: `cargo build -p desktop 2>&1 | tail -5`
Expected: builds clean.
Manual: launch via `scripts/dev/launch-test.sh --debug`, join Voice, and confirm voice still connects (no behavior change yet since the UI selector lands in Task 6; default profile Custom reproduces today's behavior). Confirm the RT warning prints if `/proc/self/limits` shows `Max realtime priority 0`.

- [ ] **Step 5: Commit**

```bash
git add desktop/src/app.rs
git commit -m "feat(voice): apply profile-derived effective DSP; surface Warning events"
```

---

### Task 6: Desktop Settings UI — profile selector + re-probe

**Files:**
- Modify: `desktop/src/ui/settings.rs` (profile dropdown, read-only preset view, Re-probe button + summary label)
- Test: run-and-observe

**Interfaces:**
- Consumes: `desktop::config::{VoiceProfile, demote_to_custom, effective_dsp, write_dsp}`, `engine::audio::classify::classify_output`, `engine::audio::rt::realtime_available`, `engine::audio::devices::list_devices`.
- Produces: full user control of the profile + a "re-analyze now" action.

- [ ] **Step 1: Add a profile selector row**

Following the existing dropdown patterns in `settings.rs` (e.g. the activation-mode selector), add a `gtk::DropDown` (or the existing combo pattern) over `["Custom", "Headset", "Speaker", "Auto"]` bound to `Settings.profile`. On change:
- set `s.profile` to the chosen variant;
- if not `Custom`, refresh the advanced toggles to **display** `effective_dsp(s, kind)` read-only (compute `kind` via `classify_output` when `Auto`, else `Unknown`), and disable those toggle widgets;
- if `Custom`, re-enable the toggles and show the stored flag values.
Persist + re-sync the session via the existing settings-apply path.

- [ ] **Step 2: Wire the demote-on-edit rule**

On any advanced filter widget (NS / AEC / AGC / VAD) change *while `s.profile != Custom`*: call `crate::config::demote_to_custom(&mut s, kind)` first (materializes the preset into the flags + sets Custom), then apply the user's specific toggle to `s`, then re-enable the toggles and persist. The result: profile becomes Custom and the flags equal "preset + this edit", overwriting the old custom (matches the spec, no undo).

- [ ] **Step 3: Add the Re-probe button + summary label**

Add a `gtk::Button` labelled "Re-probe audio" and a `gtk::Label` for the summary. On click:

```rust
            let kind = engine::audio::classify::classify_output(s.output_device.as_deref());
            let rt = engine::audio::rt::realtime_available();
            // refresh device dropdowns from a fresh enumeration:
            let _devices = engine::audio::devices::list_devices();
            let kind_str = match kind {
                engine::audio::profile::OutputKind::Headphones => "Headphones",
                engine::audio::profile::OutputKind::Speakers => "Speakers",
                engine::audio::profile::OutputKind::Unknown => "Unknown",
            };
            summary_label.set_text(&format!("Output: {kind_str} · realtime: {}", if rt { "yes" } else { "no" }));
            // If on Auto, re-apply effective immediately via the settings-apply path.
```

Re-populate the input/output device dropdowns from `_devices` using the same construction the panel already uses at build time.

- [ ] **Step 4: Build + run-and-observe**

Run: `cargo build -p desktop 2>&1 | tail -5`
Expected: builds clean.
Manual checklist (launch two instances, `scripts/dev/launch-test.sh --debug`, join Voice):
- Switch Custom→Headset→Speaker→Auto; confirm the advanced toggles show the right read-only values and AEC engages/disengages (measure with `scripts/measure/audio_delay.py`: Headset ~7–14 ms, Speaker ~20 ms).
- On a preset, flip AEC; confirm the profile flips to Custom and the toggles become editable, retaining the preset's other values.
- Auto: change the output device; confirm effective flips. Click Re-probe; confirm the summary line updates ("Output: Headphones · realtime: yes").

- [ ] **Step 5: Commit**

```bash
git add desktop/src/ui/settings.rs
git commit -m "feat(voice): Settings profile selector, read-only preset view, re-probe"
```

---

## Self-Review

**Spec coverage:**
- Profiles (Custom/Headset/Speaker/Auto) + presets → Task 1. ✅
- Edit-demotes-to-Custom + single custom slot → Task 4 (`demote_to_custom`, tests) + Task 6 wiring. ✅
- Auto = classification, Linux now / Windows `Unknown` → Task 2. ✅
- RT warning → Task 3. ✅
- Re-probe (devices + classification + RT + apply on Auto) → Task 6. ✅
- Default Custom/all-off unchanged → Task 4 (default) + Task 5 (Custom path reproduces today). ✅
- Data flow through existing `set_dsp` → Task 5. ✅
- Testing/acceptance (unit + manual 3–4 peers, audio_delay) → per-task verification. ✅

**Placeholder scan:** none — every code step has concrete code; the only `TODO`s are the intentional, documented Windows-deferral markers.

**Type consistency:** `DspConfig` fields (`echo_cancel`, `noise_suppression`, `agc`, `vad`, `high_pass`), `NsLevel` variants, `VoiceProfile`/`OutputKind` names, and `effective`/`preset_for`/`classify_output`/`realtime_available`/`demote_to_custom` signatures are used identically across tasks. Desktop↔engine enum mapping is explicit in Task 4.

**Open note for the implementer:** if the engine has any exhaustive `match` on `SessionEvent` (besides the desktop's), add a `Warning` arm there in Task 3 to keep it compiling — flagged in Task 3 Step 4.
