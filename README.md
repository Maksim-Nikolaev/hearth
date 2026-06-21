# Hearth

A self-hosted, low-latency voice + high-fidelity screenshare app for a small
group of close friends. Three independent media flows – voice, screenshare (with
audio), and webcam – over a peer-to-peer mesh, coordinated by a small Rust
server. A persistent "always available" hangout, not a federated platform.

## Status

Pre-implementation. The approved design lives in
[`docs/superpowers/specs/2026-06-21-hearth-design.md`](docs/superpowers/specs/2026-06-21-hearth-design.md).

## Architecture at a glance

- **Desktop client** – Flutter + `flutter_rust_bridge` (Windows + Linux/X11 for
  MVP; macOS if free; mobile later).
- **Media engine** – Rust + GStreamer (`webrtcbin`), three independent flows
  multiplexed over one WebRTC PeerConnection per peer. OBS-style runtime encoder
  detection (AMF/NVENC/QSV/VAAPI/VideoToolbox + software fallback).
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
| S2 | Media engine | Rust / GStreamer / `webrtcbin` |
| S3 | Desktop client | Flutter / `flutter_rust_bridge` |
| S4 | TURN relay | coturn |
| S5 | Infra & observability | Docker / Traefik / Grafana / Loki |

## License

No license chosen yet (all rights reserved by default).
