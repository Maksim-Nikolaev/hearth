# Wire mic/speaker volume to the live session

Date: 2026-06-25
Status: Approved (design)

## Goal

Make the **mic (input)** and **speaker (output)** volume sliders actually change
live audio. Today `Settings.input_volume` / `output_volume` (f64, 0.0–1.0) are
persisted and reach `app.rs`, but nothing forwards them to the engine — no
setter, no gain applied anywhere. They are cosmetic.

## Scope (decided)

- **Range:** attenuate-only **0.0–1.0** (unity at max, matching today's sliders).
  `gain ≤ 1.0` can never clip — no soft-clip/boost logic. Mic boost (0–200%) is an
  explicit non-goal for v1.
- **Both backends:** the native path (default) **and** the GStreamer `voice_udp`
  fallback.
- Applied **live** (no rebuild), and re-applied after a device-change rebuild.

## Where the gain applies

Both volumes are a simple linear gain at the existing f32 mono-frame points:

- **Mic (input) volume — pre-amp at the front of capture**, before AEC/DSP, so it
  behaves like a mic-level control; opt-in AGC (default off) normalizes after it.
  - Native: multiply the mono frame at the top of the `NativeCapture` `on_frame`
    closure in `native_voice.rs`.
  - GStreamer fallback: the same multiply in `VoiceCapture`'s `cap` appsink
    callback (`audio/capture.rs`) — one shared capture point.
- **Speaker (output) volume — gain on the rendered mix** (master speaker level,
  uniform across peers).
  - Native: a `volume` atomic on `NativePlayback`, multiplied into the summed
    output in the render loop before `soft_clip` (both backend impls —
    `native_pw.rs` and `native_wasapi.rs`).
  - GStreamer fallback: a live GStreamer `volume` element in each peer's
    `voice_udp` recv pipeline (between `opusdec`/convert and the sink).

## Plumbing

- **Engine `Session`** (`session.rs`): store `input_volume: f64` / `output_volume:
  f64` (default 1.0); add `set_input_volume(f64)` / `set_output_volume(f64)` that
  clamp to `[0.0, 1.0]`, save the value, and forward to the live path:
  - native: `NativeVoice::set_input_volume` / `set_output_volume`;
  - GStreamer: set the capture-side mic gain + each `VoiceUdpPeer`'s recv `volume`
    element.
  - On `register_voice_send` / peer add and on `rebuild_native_voice`, re-apply the
    stored volumes so new peers/instances start at the right level.
- **Native** (`native_voice.rs`): `input_volume: Arc<AtomicU32>` (f32 bits) read
  each frame in the capture closure; `NativePlayback` gains `volume: Arc<AtomicU32>`
  + `set_volume(f64)` applied in render. `NativeVoice::set_input_volume` /
  `set_output_volume` store into the atomics (default 1.0 at construction).
  `ensure_native_voice` calls the two setters immediately after building the
  instance, so the `NativeVoice::new` signature is unchanged.
- **GStreamer** (`voice_udp.rs`, `audio/capture.rs`): `VoiceUdpPeer` adds a named
  `volume` element + `set_output_volume(f64)`; `VoiceCapture` holds a shared mic
  gain (atomic) applied in the `cap` callback + `set_input_volume(f64)`.
- **Desktop** (`app.rs`): the `SettingsOutput::InputVolume` / `OutputVolume`
  handlers call `session.set_input_volume` / `set_output_volume` in addition to
  storing the setting; `apply_settings_to_session` applies both at startup.

## Components / files

- `engine/src/session.rs` — fields + setters + apply/rebuild wiring.
- `engine/src/audio/native_voice.rs` — input pre-amp + `NativeVoice` setters.
- `engine/src/audio/native/native_pw.rs`, `native_wasapi.rs` — `NativePlayback`
  output `volume` atomic + `set_volume` + render multiply.
- `engine/src/audio/capture.rs` — `VoiceCapture` mic gain + `set_input_volume`.
- `engine/src/voice_udp.rs` — per-peer `volume` element + `set_output_volume`.
- `desktop/src/app.rs` — call the session setters (handlers + startup apply).

## Testing

- Unit-test a pure `apply_gain(frame: &mut [f32], gain: f32)` helper (and that
  `gain` is clamped to `[0,1]`): silence stays silence, gain 1.0 is identity, gain
  0.0 zeroes, gain 0.5 halves.
- Device-level effect (mic quieter, speaker quieter, live slider drag) is manual
  on the dev box.

## Non-goals

- Mic/speaker **boost** above unity (0–200%).
- **Per-peer** individual volume (this is a single master in/out).
- Per-source screenshare-audio volume.
</content>
