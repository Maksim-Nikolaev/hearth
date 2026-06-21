# Hearth – Status

_Living status doc. Last updated: 2026-06-21._

Self-hosted, low-latency voice + high-fidelity screenshare + webcam for 2–3 close
friends over a P2P mesh with a small Rust control server. Stack: **pure-Rust
GTK4 + relm4 desktop client** calling the Rust media engine (GStreamer
`webrtcbin`) directly – no language bridge; Rust/Axum backend. A Flutter mobile
app is a later, separate effort (shares only backend + protocol, uses
`flutter_webrtc`). Work happens on `main`, committed locally, never pushed unless
asked.

**Media transport: per-flow PeerConnections** – one `webrtcbin` per flow (Voice /
Screenshare+audio / Webcam), chat over the WebSocket. Each flow drops and
congests independently (revised from the earlier single-bundle "Approach A").

## Milestones

| Milestone | State | Notes |
|-----------|-------|-------|
| M0–M1 backend foundation | ✅ done, green | Axum, PG18, users, argon2, JWT, `/auth/*`, admin `/users`, WS `/ws` presence |
| M2 media spike | ✅ GO (loopback) | throwaway `engine-spike/`; `vah265enc` HW HEVC; two-peer `webrtcbin` over `/tmp` |
| M3 signaling | ✅ done, green | `backend/src/signaling/` relays join/offer/answer/ice/leave over `/ws` |
| M4 networked engine (Tasks 1–4) | ✅ done, Linux loopback GO | real `engine/` crate driven by the server, no `/tmp` |
| M4 Task 5 (cross-machine) | ⏸ blocked – user-run on Windows | the Approach-A go/no-go; needs the Windows 11 box |
| M5 Rust GTK4 desktop shell | ▶ **next (design agreed, spec pending)** | relm4 app: login + presence + chat + screenshare share/view in-window; per-flow framework |

## Component state

**Backend (`backend/`)** – Axum + sqlx + PG18 (dev container, host port 5433).
Auth (JWT + argon2, admin-provisioned, revocable refresh), presence + signaling
multiplexed over `/ws`. 15 tests green. Signaling message types now come from the
shared `hearth-protocol` crate.

**Protocol (`protocol/`)** – `hearth-protocol`: `PeerInfo` / `ClientMessage` /
`ServerMessage`, both directions `Serialize + Deserialize`. Backend re-exports it;
engine depends on it directly. Single source of truth for the wire format.

**Engine (`engine/`)** – product crate (supersedes `engine-spike/`):
- `encoders` – runtime HW HEVC probe (selects `vah265enc` on this box).
- `capture` – per-OS chain; `HEARTH_CAPTURE` override; `videorate`/`videoscale`/caps.
- `signaling` – REST login → JWT WebSocket, typed send/recv (mock-WS unit test).
- `peer` – `webrtcbin` driven by the signaling client; `share`/`view` modes.
- CLI: `engine probe|share|view`, fully env-configured. Bus error/warning watch.
- **Verified:** Linux loopback through the real backend – both peers `Connected`,
  viewer linked + displayed the stream. **One video track (screenshare) only.**

## Verified vs. open

**Verified (Linux):** signaling-driven offer/answer/ICE, HW HEVC encode/decode,
single screenshare track end-to-end through the server, no-recompile env config.

**Open / not yet built:**
- **Per-flow media** – voice + webcam flows (each its own `webrtcbin`); only the
  screenshare flow exists today. M4's screenshare peer IS that flow.
- **Multi-peer mesh** – currently 1:1 (first peer). Spec wants 2–3 friends.
- **Cross-machine / cross-NAT** – latency, AMF encode, direct-ICE-vs-TURN
  (Task 5, Windows-blocked). Runbook: `engine/docs/windows-setup.md`.
- **App** – no Flutter UI yet; engine is CLI-only.
- **Backend features** – text-chat persistence, attachments (RustFS two-phase
  upload per the Lezio pattern) not started.

## Next step: M5 Rust GTK4 desktop shell (design agreed, spec pending)

A pure-Rust desktop client (GTK4 + relm4) calling the `engine` crate directly:
- login screen → `/auth/login` (token stored via the `keyring` crate)
- room / presence view (who is online)
- text chat (needs a backend slice: `messages` table/repo/migration + WS relay)
- screenshare **share / view** shown in-window via `gtk4paintablesink` → `gtk::Picture`
- the per-flow framework + flow-tagged signaling

GTK and GStreamer share the GLib main loop, so the engine needs no separate
loop/thread and no bridge; tokio runs the WS/REST off-thread, events reach the UI
as relm4 messages. The engine's CLI `peer::run` is refactored into a library API
(`Session`, `Flow`, `FlowPeer`); `probe/share/view` stay working so Task 5 is
unaffected. New root Cargo workspace (`protocol`, `engine`, `backend`, `desktop`).

**Out of M5 / next:** voice + webcam flows (additive to the framework),
multi-peer mesh, theming, packaging/auto-update, TURN/coturn (M6 = Traefik proxy
+ coturn relay).

Design was brainstormed 2026-06-21; spec to be written to
`docs/superpowers/specs/2026-06-21-hearth-m5-desktop-shell-design.md`.

## How to resume / run

- Source Rust: `. "$HOME/.cargo/env"`. Dev DB: `docker compose -f compose.dev.yml up -d postgres` (host port 5433).
- Backend: `DATABASE_URL=postgres://hearth:hearth@localhost:5433/hearth JWT_SECRET=dev-only-change-me-min-32-bytes-long-secret cargo run` (listens `0.0.0.0:8080`).
- Seed users: `password::hash` + `users::repository::create` (admin endpoint also exists).
- Engine loopback: backend up + two users, then `engine view` and `engine share` (see `engine/README.md`).
- Plans: `docs/superpowers/plans/`. Spec: `docs/superpowers/specs/2026-06-21-hearth-design.md`.
