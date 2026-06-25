# Hearth media engine

Cross-platform Rust media engine for Hearth: native low-latency **voice** and
hardware-encoded P2P **screenshare**, driven by the Hearth WebSocket signaling
server. The desktop client (`desktop/`) links it directly as a library; the CLI
below covers the screenshare flow. (A future mobile app would wrap it via
`flutter_rust_bridge`.)

## Modules

- `encoders` – runtime HW HEVC encoder probe (AMF / VA-API / NVENC / QSV /
  VideoToolbox, software fallback).
- `capture` – per-OS screen-capture sub-pipeline (Linux `ximagesrc`,
  Windows `d3d11screencapturesrc`).
- `signaling` – REST login (JWT) + typed WebSocket client (`ClientMessage` /
  `ServerMessage` from `hearth-protocol`).
- `peer` – `webrtcbin` pipeline wired to the signaling client; offerer/capture
  in `share` mode, answerer/display in `view` mode.
- `audio` – **native low-latency voice** (default; GStreamer is the auto-fallback).
  `audio/native/` is per-platform device I/O — pipewire-rs on Linux
  (`native_pw.rs`), WASAPI `IAudioClient3` on Windows (`native_wasapi.rs`) —
  behind one `NativeCapture`/`NativePlayback` API. `native_voice` runs the
  capture → DSP → Opus chain; DSP is pure-Rust (`speex_aec` / `webrtc_aec` echo
  cancellation, `nnnoiseless` NS, `earshot` VAD, envelope AGC, activation gate).
  `dsp` / `capture` / `monitor` are the GStreamer `pulsesrc`/`wasapi2` fallback
  path; `voice_udp` is the shared raw-RTP/Opus-over-UDP transport (no `webrtcbin`).

## CLI

```bash
engine probe          # list encoders + show selected encoder and capture chain
engine share          # capture this screen and send to the first room peer
engine view           # receive and display a peer's screen
```

`share`/`view` read configuration from the environment. The capture/quality
knobs let one binary adapt per machine with no recompile (important for the
Windows run, where element names may differ):

| var                  | default                  | effect |
|----------------------|--------------------------|--------|
| `HEARTH_HTTP`        | `http://127.0.0.1:8080`  | backend REST base |
| `HEARTH_WS`          | `ws://127.0.0.1:8080`    | backend WebSocket base |
| `HEARTH_USER`        | (required)               | login username |
| `HEARTH_PASS`        | (required)               | login password |
| `HEARTH_ROOM`        | `main`                   | room to join |
| `HEARTH_CAPTURE`     | per-OS default           | override the capture sub-pipeline entirely |
| `HEARTH_FPS`         | `30`                     | pinned framerate |
| `HEARTH_WIDTH`/`HEIGHT` | native              | pin resolution (set both, e.g. 1920/1080) |
| `HEARTH_BITRATE_KBPS`| `8000`                   | encoder bitrate hint (kbps) |
| `HEARTH_TURN`        | (none)                   | TURN relay, e.g. `turn://user:pass@host:3478` |

### Latency bench

For reproducible glass-to-glass latency (phone-camera stopwatch), share a clock
instead of the screen — identical on Linux and Windows:

```bash
HEARTH_CAPTURE="videotestsrc is-live=true ! timeoverlay ! videoconvert" engine share
```

### Cross-machine (Windows ↔ Linux)

See [`docs/windows-setup.md`](docs/windows-setup.md) for the full Task 5 runbook
(toolchain, GStreamer MSVC install, element-name fallbacks, measurements, coturn).

## Verification log

### Task 4 – networked loopback (Linux/X11, 2026-06-21)

Same-machine loopback over the real Hearth backend (no `/tmp` files): backend on
`127.0.0.1:8080`, two seeded users (`alice`, `bob`), `view` + `share` peers.

**Result: GO.** Both peers reached `connection-state: Connected`; the viewer
printed `incoming stream linked -> displaying` and a window showed the shared
screen. Encoder selected: `vah265enc` (AMD VA-API HEVC). This confirms the full
signaling-driven path (REST login → JWT WebSocket → offer/answer/ICE relay →
`webrtcbin` media) end-to-end through the server.

### Task 5 – cross-machine (Windows ↔ Linux)

Pending – run on the Windows boxes. Record per the plan:
glass-to-glass latency (target < ~150 ms LAN), 1080p/60 legibility under motion,
steady-state bitrate / CPU% / GPU encoder load, and whether direct ICE connects
or a TURN relay is needed.
