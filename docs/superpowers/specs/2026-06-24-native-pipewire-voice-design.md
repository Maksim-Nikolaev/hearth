# Native PipeWire voice capture/playback on Linux

Date: 2026-06-24
Status: Approved (design)

## Goal

Give Linux a native, low-latency voice device path (pipewire-rs) that mirrors the
Windows native WASAPI path, and establish a single policy on both platforms:

> Use the best/most robust audio driver by default; fall back to the generic
> GStreamer driver automatically when the native one fails to start.

The motivating defect: the Linux GStreamer `pulsesrc` path measures sub-10 ms
mouth-to-ear at the start of a session but **drifts to ~70 ms over a long
session**. Root cause is the dynamic PipeWire graph quantum (default 1024 @
48 kHz ≈ 21 ms, `force-quantum=0`) plus the PulseAudio compat-shim buffer growing
under load. A native pipewire-rs stream with a **pinned small quantum** bounds the
capture period and removes the drift.

## Non-goals

- macOS stays on the GStreamer `voice_udp` path (unchanged).
- No change to the wire transport (raw RTP-less `[seq|opus]` over UDP for the
  native path; GStreamer RTP/Opus for the fallback) or to signaling.
- No DSP algorithm changes; the existing pure-Rust suite (nnnoiseless / earshot /
  aec-rs / speexdsp) is reused as-is.

## Background: why this is mostly reuse

`native_voice.rs` (the Phase-2 send loop: AEC → VAD → NS → AGC → gate → Opus →
UDP, plus per-peer recv→decode→mixer-lane and the mic-test monitor) depends
**only** on the public API of two device types:

- `NativeCapture::start(device: Option<String>, on_frame: FnMut(&[f32]))`
- `NativePlayback::start(device)`, `.push(source, &[f32])`, `.far_end()`,
  `.remove_source(id)`
- shared helpers `SAMPLE_RATE`, `soft_clip`, `rms_dbfs`, `ramp_gain`,
  `MAX_LANE_SAMPLES`.

None of that orchestration is Windows-specific. Providing a second device backend
with the identical API makes the whole Phase-2 voice stack run on Linux unchanged.

## Architecture

### Module layout

- `engine/src/audio/native_wasapi.rs` — `#[cfg(windows)]`. The current `native.rs`
  device I/O (`NativeCapture`/`NativePlayback`/`DeviceStream`/`open_device`/
  `capture_loop`/`playback_loop`) moves here verbatim.
- `engine/src/audio/native_pw.rs` — `#[cfg(target_os = "linux")]`. New pipewire-rs
  `NativeCapture`/`NativePlayback` with the same signatures.
- `engine/src/audio/native/mod.rs` (or keep `native.rs` as the umbrella) —
  re-exports the right backend per cfg and holds the **platform-independent**
  pieces in one place: `soft_clip`, `rms_dbfs`, `NativeMonitor`, `SAMPLE_RATE`,
  `MAX_LANE_SAMPLES`, `FAR_END_CAP`, and the Opus round-trip test.
- `native_voice.rs` — `#[cfg(windows)]` gate widens to
  `#[cfg(any(windows, target_os = "linux"))]`. No code change beyond the gate.

Backend selection is a compile-time `cfg` re-export, so `native_voice.rs` and
`NativeMonitor` see one concrete `NativeCapture`/`NativePlayback` type per target.

### PipeWire backend (`native_pw.rs`)

- **Threading:** each of capture and playback owns a `pw::ThreadLoop` plus one
  `pw::Stream`, mirroring the per-stream WASAPI thread. `start()` connects the
  stream and blocks on a ready handshake (same `mpsc::channel::<Result<(),String>>`
  pattern the WASAPI backend uses), so a connect failure is returned to the caller
  (which drives auto-fallback — see Selection).
- **Format:** request F32 / 48 kHz / mono via SPA `audio/raw`. PipeWire converts
  from the device's native rate/channels, replacing the WASAPI AUTOCONVERTPCM path
  and the GStreamer audioconvert/audioresample stages.
- **Pinned quantum:** stream property `node.latency = "256/48000"` (~5.3 ms) by
  default, overridable via env `HEARTH_PW_QUANTUM` (e.g. `480/48000`) to sweep on
  real hardware. This pins the capture period and is the drift fix.
- **Capture `process` callback:** dequeue buffer → downmix to mono f32 →
  `on_frame(&mono)` (runs the same DSP+Opus chain as WASAPI, on PipeWire's RT
  thread).
- **Playback `process` callback:** sum the mixer lanes into the output buffer,
  trim lanes to `MAX_LANE_SAMPLES` (~20 ms), tap the rendered mono into the
  `far_end` ring (the AEC reference). Lane/ring semantics identical to WASAPI.
- **Device selection:** the Settings picker already stores `node.name` (e.g.
  `alsa_input.usb-Logitech_PRO_X_2_LIGHTSPEED_…mono-fallback`). Pass it straight
  through as the stream prop `target.object`. Empty / unresolved id → PipeWire
  default node (log a note, matching the WASAPI default fallback).
- **AEC far-end:** comes from `NativePlayback`'s rendered mix ring, exactly like
  Windows. The native path therefore does **not** open the GStreamer
  `<sink>.monitor` capture pipeline — one fewer moving part, and it sidesteps the
  self-echo class of bug.
- **RT scheduling:** pipewire-rs runs the `process` callback on its rtkit-granted
  RT thread automatically. `rt.rs::realtime_available()` is queried at startup to
  emit an informational log / `SessionEvent` Warning when RT is unavailable
  (consistent with the existing DSP-profile warnings).

### Selection and auto-fallback (both platforms)

Policy, symmetric on Windows and Linux:

1. **Default:** attempt the native backend (WASAPI on Windows, PipeWire on Linux).
2. **Auto-fallback:** if native construction fails (`NativeVoice::new` /
   `NativeCapture`/`NativePlayback::start` returns `Err`), log a Warning and use
   the generic GStreamer `voice_udp` path for the remainder of the session,
   instead of surfacing a fatal error.
3. **Manual override:** `HEARTH_GSTREAMER_VOICE=1` forces the generic path from
   the start (skips the native attempt entirely).

Implementation:

- `native_voice_selected()` and `ns_wet_permille()` gates widen from
  `#[cfg(windows)]` to `#[cfg(any(windows, target_os = "linux"))]`, same bodies.
- A per-session flag (e.g. `native_voice_failed: bool`) records a failed native
  attempt so the session stops retrying native and stays on GStreamer until
  rebuild (device change / rejoin).
- `ensure_native_voice()` returning `None`/`Err` becomes a **fall-through** to the
  GStreamer branch in `voice_offer` / `voice_on_offer`, not an emitted error. This
  is the one behavioural change to the existing Windows path.
- The `#[cfg(target_os = "windows")]` blocks in `session.rs` (the `native_voice`
  field, `ensure_native_voice`, `rebuild_native_voice`, and the offer / answer /
  stop / device-change call sites) widen to
  `#[cfg(any(windows, target_os = "linux"))]`. The GStreamer branch remains as the
  fallback arm in each.

### Backend indicator (UI)

The app surfaces which voice backend is active so the auto-fallback is visible
rather than silent.

- New engine event `SessionEvent::VoiceBackend(VoiceBackendKind)` where
  `VoiceBackendKind` is `Native` (WASAPI / PipeWire) or `Generic` (GStreamer).
  Emitted once when the voice transport is first constructed for a session, and
  again on `rebuild_native_voice` (device change / rejoin), so the indicator
  always reflects reality — including the case where a native attempt failed and
  the session fell through to generic.
- Bridged in `engine/src/lib.rs` to a `WorkspaceInput`, stored in desktop UI
  state, and shown as a small read-only line in the Settings voice section
  (`desktop/src/ui/settings.rs`), e.g. **"Audio engine: Native (PipeWire)"** /
  **"Audio engine: Generic (GStreamer)"**. Follows the existing read-only
  status-line pattern (alongside the DSP-profile / RT-probe surfaces).
- The kind is derived at the single selection site, so it cannot disagree with
  the transport actually built.

### Dependencies / build

- Add `pipewire = "0.8"` under
  `[target.'cfg(target_os = "linux")'.dependencies]` (binds system
  `libpipewire-0.3`).
- New build dependency `libpipewire-0.3-dev` (Kubuntu). Record in the build-env
  memory; fail the build with a clear message if the lib is absent.

## Data flow (Linux native, per session)

```
mic node ──pw::Stream(capture, 256/48000, F32/48k/mono)──► on_frame()
   └─ AEC(far_end ring) → VAD → NS → AGC → gate/ramp → Opus.encode_float
        └─ [seq u16 | opus] ──UDP──► each peer

peer UDP ──► recv thread → Opus.decode_float (+PLC) → NativePlayback.push(lane)
NativePlayback render ──► sum lanes → soft_clip → pw::Stream(playback) → speaker node
                              └─ tap rendered mono → far_end ring (AEC reference)
```

## Testing & verification

- **Unit-testable (CI):** pure helpers — downmix averaging, lane trim to
  `MAX_LANE_SAMPLES`, `far_end` cap, the negotiated-format → frame-count mapping,
  and the existing Opus low-delay round-trip test. Factor any new logic into pure
  functions so it is testable without a server.
- **Not CI-testable:** device I/O needs a live PipeWire server, so it is verified
  manually on the dev box. Per the standing rule, the user runs the real voice
  test; the agent does not launch capture/voice itself.
- **Acceptance criterion (the point of the work):** add a `[native]` log of the
  capture period and the deepest mixer-lane backlog every ~2 s. A long-session
  call (30+ min) must keep mouth-to-ear latency **bounded** (no drift toward
  70 ms), versus the GStreamer baseline.

## Risks & mitigations

- **pipewire-rs 0.8 vs system libpipewire 1.6.2** — the crate binds the
  ABI-stable libpipewire-0.3 API; the 1.6 server is compatible.
- **Stale stored device id** — fall back to the PipeWire default node, logged.
- **RT unavailable** (no rtkit, RLIMIT_RTPRIO 0) — capture still runs, just not
  RT; emit a Warning. Drift may be larger but the pinned quantum still helps.
- **Regression surface** — the GStreamer `voice_udp` + `pulsesrc` path is left
  fully intact as the fallback and manual-override escape hatch.

## Open implementation details (resolve during planning)

- Exact pipewire-rs stream-property keys for `target.object` and `node.latency`
  (`PW_KEY_*` constants vs string props) on the 0.8 API.
- Whether `NativeMonitor` and the shared helpers move into a `native/mod.rs`
  directory module or stay in a slimmed `native.rs` umbrella file.
</content>
</invoke>
