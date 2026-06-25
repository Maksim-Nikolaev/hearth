# Hearth

A self-hosted, low-latency voice + high-fidelity screenshare app for a small
group of close friends. Three independent media flows – voice, screenshare (with
audio), and webcam – over a peer-to-peer mesh, coordinated by a small Rust
server. A persistent "always available" hangout, not a federated platform.

> **Read [`docs/VISION.md`](docs/VISION.md) first.** The north star: state-of-the-art,
> native, **lowest-latency** audio (< 50 ms, Mumble/TeamSpeak-class) and
> **OBS-style per-source** screenshare (window/app/game video **with** matching
> audio) — no Discord compromises. Voice is **off WebRTC**: raw RTP/Opus over UDP
> with native per-platform device I/O (PipeWire on Linux, WASAPI on Windows),
> measured **~6–14 ms** on Linux. See
> [`docs/research/voice-transport.md`](docs/research/voice-transport.md).

## Status

Implemented and verified through M7 (auth, presence, chat, group voice mesh,
multi-sharer screenshare with app/system audio, voice DSP) plus the **audio
workstream: complete** — native low-latency voice on Linux (pipewire-rs) and
Windows (WASAPI), best-driver default with auto-fallback to GStreamer, and
user-selectable/tunable echo cancellation (Speex / WebRTC AEC3). The Windows 11
native build (X11-only paths gated behind `cfg(target_os = "linux")`;
`d3d11screencapturesrc` + runtime AMF/NVENC/QSV) is merged into `main`; see
[`engine/docs/windows-setup.md`](engine/docs/windows-setup.md).

The live status doc is [`docs/STATUS.md`](docs/STATUS.md); the approved design
lives in
[`docs/superpowers/specs/2026-06-21-hearth-design.md`](docs/superpowers/specs/2026-06-21-hearth-design.md).
**Next major workstream:** Wayland screenshare GPU capture (portal ScreenCast +
`pipewiresrc` + DMABuf→VA).

## Architecture at a glance

- **Desktop client** – pure-Rust **GTK4 + relm4**, calling the Rust media engine
  directly (no language bridge). Linux/X11 today; Windows in progress. A Flutter
  *mobile* app is a later, separate effort that shares only the backend +
  protocol (via `flutter_webrtc`).
- **Media engine** – Rust. **Voice** is native low-latency: per-platform device
  I/O (pipewire-rs / WASAPI `IAudioClient3`) → pure-Rust DSP → Opus →
  **raw RTP/Opus over UDP** (no `webrtcbin`), with GStreamer `pulsesrc`/`wasapi2`
  as the auto-fallback. **Screenshare + webcam** still run **one GStreamer
  `webrtcbin` per flow** (each drops/congests independently); chat rides the
  WebSocket. OBS-style runtime encoder detection
  (AMF/NVENC/QSV/VAAPI/VideoToolbox + software fallback).
- **Backend** – Rust/Axum: auth (JWT + argon2), presence, text chat,
  attachments, and WebSocket signaling. CLELO-style layered structure.
- **Storage** – Postgres 18 (uuidv7) + RustFS (S3-compatible) for attachments.
- **Infra** – Docker Compose, Traefik (TLS), coturn (TURN), Grafana/Loki,
  sops + age secrets.

Media stays peer-to-peer and bypasses the server; the server only brokers the
handshake.

## Subsystems

| # | Subsystem | Stack |
|:--|:--|:--|
| S1 | Backend server | Rust / Axum / sqlx / Postgres / RustFS |
| S2 | Media engine | Rust; native audio I/O (pipewire-rs / WASAPI) + RTP/UDP voice; GStreamer `webrtcbin` for video |
| S3 | Desktop client | Rust / GTK4 + relm4 |
| S4 | TURN relay | coturn |
| S5 | Infra & observability | Docker / Traefik / Grafana / Loki |

## Development

```sh
# Backend + Postgres (containerised) for local dev:
docker compose -f compose.dev.yml up

# Or run the workspace crates directly (needs Rust + GStreamer; the desktop
# crate also needs GTK4). See engine/docs/windows-setup.md for the Windows box.
cargo run -p hearth-backend     # API + signaling on :8080
cargo run -p desktop            # GTK4 client
```

## License

No license chosen yet (all rights reserved by default).
