# Voice DSP profiles + RT-safety — design

_2026-06-24. Sub-project 1 of the voice latency/quality workstream. Spec 2 (voice
transport hardening: encryption + NAT traversal) is separate. Screenshare is out
of scope._

## Goal

A robust, cross-platform **DSP profile layer above the existing per-platform DSP
engines**. Priorities, in order: DSP **quality**, robustness on both platforms,
then shaving latency where it is free. Even all-filters-on is ~20 ms today
(`docs/findings/voice-latency-linux.md`), so this pass is about giving users the
right processing for their setup — not chasing milliseconds.

Chosen approach (**C**): keep the best DSP engine per platform — Linux on
`webrtc-audio-processing` (gold-standard AEC, already works), Windows on the
pure-Rust suite (`nnnoiseless`/`earshot`/`aec-rs`, already works) — and build the
profile switch, device classification, and RT-safety check as **one cross-platform
layer** behind the existing `audio::dsp` / `Session::set_dsp` seam. Engine
convergence onto one codebase is an explicit *later* decision, not this spec.

## Non-goals

- No change to the DSP engines themselves (no porting, no new C build risk).
- No new scheduling subsystem — RT-safety is a warning only in v1.
- Auto device classification on **Windows** is deferred (returns `Unknown`).
- No encryption / NAT / screenshare (separate specs).

## Profiles

A `VoiceProfile` with four variants:

- **Custom** — the user's hand-tuned `DspConfig`. **Default**, and defaults to
  all-off (preserves today's behavior exactly).
- **Headset** — `echo_cancel=false, noise_suppression=Moderate, agc=true,
  high_pass=true, vad=true`. Lowest latency (~7–14 ms).
- **Speaker** — `echo_cancel=true, noise_suppression=Moderate, agc=true,
  high_pass=true, vad=true`. Full processing (~20 ms).
- **Auto** — resolves to a preset from output-device classification:
  `Headphones → Headset`, clear speaker → `Speaker`, `Unknown` → `Headset`
  (safe low-latency default).

Presets are compile-time constants, so they can never produce an invalid config.
The starting preset values are tunable later without changing the design.

## State model & the "edit demotes to Custom" rule

Persisted state is two fields: `profile: VoiceProfile` and a single
`custom: DspConfig` slot.

The **effective** `DspConfig` driving the engine:

```
effective = match profile {
    Custom  => custom,
    Headset => HEADSET_PRESET,
    Speaker => SPEAKER_PRESET,
    Auto    => preset_for(classify_output()),  // Headphones→Headset, Speaker→Speaker, Unknown→Headset
}
```

Interaction rules (single source of truth, no undo):

- **Selecting a preset** drives `effective` but **never mutates `custom`** —
  presets are read-only views.
- **Switching back to Custom with no edits** → the untouched `custom` slot.
- **Editing any filter while a preset/Auto is active** → atomically
  `profile = Custom` and `custom = <currently-visible values, i.e. the preset +
  the edit>`. The previous `custom` is overwritten and lost, by design. Auto then
  stops reacting to device changes (you are now Custom).

So `custom` is always "the last hand-tuned config," and presets are momentary
lenses on top of it.

## Components

1. **`VoiceProfile` + presets + resolver** (`engine/src/audio/`, e.g.
   `profile.rs`). Pure functions: `effective(profile, custom, classification)` and
   `preset_for(classification)`. Fully unit-testable, no I/O.

2. **`OutputClassifier`** — `classify(output_device) -> OutputKind` where
   `OutputKind = Headphones | Speakers | Unknown`.
   - **Linux (now):** inspect the `gst::Device` / PipeWire-Pulse node properties
     for the active output (form-factor / sink port type: `headphones`, `speaker`,
     `headset`, etc.). Parsing is isolated behind a pure helper that takes the
     property map, so it is unit-testable from sample descriptors.
   - **Windows (deferred):** returns `Unknown`; `// TODO: WASAPI
     PKEY_AudioEndpoint_FormFactor`.
   - Never errors; any failure → `Unknown`.

3. **RT-scheduling check** — `engine` startup probe. Linux: whether the audio path
   has realtime scheduling (PipeWire RT). Windows: MMCSS "Pro Audio". On absence,
   emit `SessionEvent::Warning(..)`. Warning only; no audio-path change in v1.

4. **Re-probe action** — `Session::reprobe_audio()`: re-enumerate devices
   (refresh Settings dropdowns), re-run `OutputClassifier` on the current output,
   re-run the RT check, and return a short summary (e.g. *"Output: Headphones ·
   realtime: yes"*). If `profile == Auto`, recompute and apply `effective`
   immediately. This is the explicit "re-analyze now" escape hatch and the only
   way to refresh classification when not on Auto.

5. **Desktop Settings UI** — a Profile selector (Custom / Headset / Speaker /
   Auto) above the existing per-filter advanced toggles. In a preset/Auto the
   toggles show the effective values **read-only**; touching one applies the
   "edit demotes to Custom" rule. A **Re-probe audio** button calls
   `reprobe_audio()` and shows the summary line.

## Data flow

On any of {profile change, advanced-toggle edit, output-device change, voice
join, re-probe}: compute `effective` → existing `Session::set_dsp(effective)`
(Linux `VoiceCapture::set_config`; Windows native setters). `Auto` re-resolves on
output-device change so unplugging a headset flips to Speaker live. Activation
mode (mute/PTT/VAD/always) stays orthogonal and unchanged.

## Integration points (existing symbols)

- `engine/src/audio/dsp.rs` — `DspConfig { echo_cancel, noise_suppression:
  NsLevel, agc, vad, high_pass }`, `NsLevel { Off, Low, Moderate, High }`.
- `engine/src/session.rs` — `set_dsp`, `set_output_device`, `set_input_device`,
  `SessionEvent`. New: `reprobe_audio`, profile state, RT-check on start.
- `engine/src/audio/devices.rs` — `list_devices()` / `AudioDevice`; classification
  reads `gst::Device` properties from the same monitor.
- `desktop/src/config.rs` — `Settings`; add `profile: VoiceProfile` and treat the
  existing NS/AEC/AGC/VAD/HPF flags as the `custom` slot.
- `desktop/src/app.rs:553` — where `set_dsp(DspConfig{..})` is built from Settings;
  becomes "compute effective from (profile, custom) then set_dsp".
- `desktop/src/ui/settings.rs` — profile selector + read-only preset view +
  Re-probe button.

## Error handling

- Classification failure / `Unknown` → Headset preset; never blocks a call.
- Windows Auto → `Unknown` → Headset; selecting Auto there is harmless.
- RT absent → warning event only.
- Presets are constants; profile/device recompute is idempotent.

## Testing & acceptance

**Unit:**
- `effective()` for every profile × classification (incl. `Unknown`).
- The edit-demotes-to-Custom-and-overwrites transition.
- Linux classifier parsing: sample property maps → Headphones/Speaker/Unknown.
- RT-check parsing (mocked).

**Manual / integration:**
- Switch profiles mid-call; confirm AEC engages/disengages, measured with
  `scripts/measure/audio_delay.py` (Headset ~7–14 ms, Speaker ~20 ms).
- Auto: change/unplug output → effective flips live; Re-probe updates the line.
- Edit a toggle on a preset → demotes to Custom, old custom overwritten.
- **3–4 local peers** simultaneously (acceptance gate); RT warning shows when RT
  is unavailable.

**Acceptance criteria:**
- Existing users unchanged (default Custom / all-off).
- Profiles apply live with no call drop.
- Auto classifies headphones vs speakers correctly on Linux.
- Windows Auto falls back to Headset without error.
- Re-probe refreshes devices + classification + RT summary.
