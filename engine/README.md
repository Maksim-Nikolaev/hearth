# Hearth media engine

Cross-platform Rust media engine for Hearth: hardware-encoded P2P screenshare
driven by the Hearth WebSocket signaling server (M3 protocol). This is product
code – the future S2 engine that `flutter_rust_bridge` will wrap (it supersedes
the throwaway `engine-spike/`).

## Modules

- `encoders` – runtime HW HEVC encoder probe (AMF / VA-API / NVENC / QSV /
  VideoToolbox, software fallback).
- `capture` – per-OS screen-capture sub-pipeline (Linux `ximagesrc`,
  Windows `d3d11screencapturesrc`).
- `signaling` – REST login (JWT) + typed WebSocket client (`ClientMessage` /
  `ServerMessage` from `hearth-protocol`).
- `peer` – `webrtcbin` pipeline wired to the signaling client; offerer/capture
  in `share` mode, answerer/display in `view` mode.

## CLI

```bash
engine probe          # list encoders + show selected encoder and capture chain
engine share          # capture this screen and send to the first room peer
engine view           # receive and display a peer's screen
```

`share`/`view` read configuration from the environment:

| var          | default                  |
|--------------|--------------------------|
| `HEARTH_HTTP`| `http://127.0.0.1:8080`  |
| `HEARTH_WS`  | `ws://127.0.0.1:8080`    |
| `HEARTH_USER`| (required)               |
| `HEARTH_PASS`| (required)               |
| `HEARTH_ROOM`| `main`                   |

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
