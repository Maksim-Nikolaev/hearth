# Hearth – Status

_Living status doc. Last updated: 2026-06-23._

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
| M5 Rust GTK4 desktop shell | ✅ done, verified | relm4 app: login + presence + chat + voice (engine) + screenshare share/view **in-window**; per-flow framework. 9 tasks, all committed on `main` |
| M6 Discord group experience | ✅ done, verified | 3-pane shell (channels+self-panel \| stage+chat \| members); group **voice mesh** (smaller-UUID offerer); multi-sharer screenshare + Watching switcher behind a `ScreenTransport` seam. 7 tasks on `main`. Two-instance live run: voice mesh + bob renders alice's screenshare on the stage. |
| M7 voice processing + advanced screenshare | ✅ implemented; voice live-verified, UI verification pending | Audio device enumeration; in-process voice DSP (`webrtc-audio-processing` crate, bundled build); activation gate (mute > ptt > vad > always); single mic-capture → DSP → fan-out pipeline with speaker-monitor AEC reference; standalone mic-test monitor; X11 global PTT (`XGrabKey`); screenshare source/quality/preview + app/system audio via PipeWire; desktop Settings model + Voice settings page + Screen Share picker. 12 tasks on `main`. Group voice live-verified two-instance both directions (see verification log). Settings UI + share picker built and unit-tested but **live-verification pending (human)**. |

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

## M5 done (2026-06-22)

Pure-Rust desktop client (`desktop/`, GTK4 + relm4) calling `engine` directly, in
a root Cargo workspace (`protocol`, `engine`, `backend`, `desktop`). Verified with
two live instances against the backend: login (token via `keyring`), presence,
text chat (+ backend `messages` slice), and **screenshare displayed in-window**
via `gtk4paintablesink` → `gtk::Picture`. Engine refactored to a library API
(`Session` / `Flow` / `FlowPeer`, per-flow `webrtcbin`s, `SessionEvent`); voice
flow loopback-verified at the engine level; mute/deafen + Stop wired. CLI
`probe/share/view/call/listen` still work, so M4 Task 5 is unaffected. Spec +
plan: `docs/superpowers/{specs,plans}/2026-06-21-hearth-m5-desktop-shell*`.

Runtime note: in-window video needs `gtk4paintablesink` from `gst-plugins-rs`,
built locally and installed to `~/.local/lib/hearth-gst-plugins/`; the desktop
app prepends it to `GST_PLUGIN_PATH` automatically.

## M6 done (2026-06-22)

Discord-style group experience on the M5 shell. Protocol gained voice/share
messages; the backend gained a **voice sub-room** (membership roster + join/leave
notify + share relay, integration-tested in `tests/voice.rs`). The engine
`Session` decodes `self_id`/`self_name` from the JWT, builds a **group voice
mesh** (one Voice `FlowPeer` per member, smaller-`Uuid` peer offers –
`should_offer`), and fans screenshare to each voice member behind a
**`ScreenTransport`** seam (`P2pTransport` now, SFU later). The desktop monolith
split into relm4 components: `app.rs` (root: `Session` + routing) +
`ui/{login,workspace,channels,self_panel,stage,chat,members}.rs` + `theme.rs`
(dark CSS). 7 tasks, one commit each, on `main`. TDD for protocol/backend/engine;
UI run-and-observe (see `desktop/README.md` M6 T5–T7).

**Verified (two live instances, synthetic capture):** dark 3-pane shell; presence
+ chat; **join Voice → group voice mesh connects**; **Share → the viewer's stage
renders the sharer's screen** (live `timeoverlay`) under a **Watching** switcher,
with chat and voice running concurrently.

**Known limitation (screenshare, same as the SFU gap):** `Offer/Answer/Ice` carry
only `(peer, flow)` and the engine keys flows by `(peer, Flow)`, so two peers
**sharing to each other simultaneously** collide on `(other, Screen)` and only one
direction connects. The designed multi-sharer case (several share, others watch –
distinct peer pairs) is unaffected. A per-stream id in the protocol fixes it;
deferred alongside the screenshare SFU.

## M7 done (2026-06-23)

Voice processing, device selection, and advanced screenshare. All 12 tasks committed on `main`; engine tasks TDD, desktop tasks run-and-observe. No backend or protocol changes.

**Engine additions:**

- `engine::audio::devices` – `DeviceMonitor`-based enumeration of audio sources and sinks; `pulsesrc`/`pulsesink` replacing `autoaudiosrc`/`autoaudiosink` with a selectable `device=` (falls back to default).
- `engine::audio::dsp` – wraps the `webrtc-audio-processing` Rust crate (bundled build via autotools): AEC / NS-level / AGC / VAD / high-pass, processing 10 ms interleaved i16 frames at 48 kHz; config applied live.
- `engine::audio::capture` – single mic-capture → DSP bridge → fan-out send branch (`appsink` PCM → `dsp.process_capture_frame()` → `appsrc`); speaker-monitor `appsink` tap feeds `dsp.process_render_frame()` as the AEC reference; `level` element drives the sensitivity meter.
- `engine::audio::monitor` – standalone mic-test pipeline (capture → DSP → level → playback) for testing the mic without being in a call.
- `engine::hotkey` – X11 global push-to-talk via `XGrabKey` (x11rb/XCB); in-app GTK handler as fallback.
- `engine::screen::sources` – X11 `_NET_CLIENT_LIST` window enumeration (id, title, thumbnail) + per-monitor entries; `ximagesrc` for whole screen or `ximagesrc xid=<win>` for a window.
- `engine::screen::capture` – resolution / fps / content-type quality controls; `tee` for local preview via `gtk4paintablesink`.
- `engine::screen::audio` – PipeWire node listing with venmic-style filters (exclude own pid, virtual/loopback nodes); builds a stereo 48 kHz DSP-off Opus audio track for the Screen flow from `pipewiresrc` (specific app) or `pulsesrc <sink>.monitor` (entire system).
- Activation gate: precedence mute > push-to-talk > VAD > always-on; all drive the existing `mic_valve`.

**Desktop additions:**

- `config.rs Settings` model – device pickers, NS/AEC/AGC/VAD flags, sensitivity, activation mode, PTT key, share resolution/fps/content-type/audio-source; persisted to local TOML/JSON config.
- `ui/settings.rs` – Settings window with Voice page: device dropdowns, Mic Test + level meter, NS/AEC/AGC/VAD toggles, sensitivity slider, activation mode selector, PTT keybind capture.
- `ui/screenshare_picker.rs` – source grid (screens + windows, thumbnails), full-res live preview, resolution/fps/content-type rows, audio-source dropdown (None / Entire System / per-app list), Go Live button.
- `ui/meter.rs` – reusable `gtk::DrawingArea` level meter.

**Verified live (2026-06-23):** group voice connects both directions and is audible through the new single-capture DSP path (two instances, Opus 64 kbps fullband confirmed in encoder log). Perceived lower quality is the expected effect of DSP defaults (NS/AGC/AEC all on); the AEC monitor-reference cancels your own replayed voice when both instances run on the same machine (a single-machine test artifact, not a regression in normal use).

**Live-verification pending (human):** Voice settings UI (device switch, Mic Test meter, DSP toggles, PTT key) and Screen Share picker (source/quality/audio-source/preview, screenshare app/system audio).

**Known limitations / follow-ups:**

- Mic/speaker volume sliders persist to config but are not applied to the live session (no engine volume setter yet).
- `ShareAudio::App` choice is not persisted across sessions.
- `webrtcdsp` GStreamer element unavailable on Ubuntu Noble – hence the in-process crate approach.
- DSP defaults (NS/AGC/AEC on) can sound heavily processed; per-user tuning is the intended path.
- The M6 mutual same-pair screenshare limitation still stands (deferred with the screenshare SFU).

## Next candidates

- **M4 Task 5** – cross-machine Windows↔Linux measurement (user-run; see
  `engine/docs/windows-setup.md`).
- **Voice + webcam in the UI** – voice works in the engine; wire the call UX and
  add the webcam flow (additive to the per-flow framework).
- **Multi-peer mesh** – media is 1:1 today (targets the first peer); presence
  already shows everyone.
- **Polish** – theming, scroll-to-bottom chat, token-restore UX. (The startup
  GtkStack warning is fixed; presence verified race-free on simultaneous join.)
- **Screenshare SFU** – server-side fan-out for 5–8 viewers, dropping into the
  `ScreenTransport` seam; also fixes mutual same-pair screenshare via a per-stream
  id in the protocol (see the M6 limitation above).
- **Multiple voice channels / webcam flow** – the structure supports more Voice
  channels and the per-flow `Webcam` variant; both deferred from M6.
- **Deployment (later)** – Traefik proxy + coturn relay; packaging/auto-update.
- **Post-M5 fixes landed:** re-share after Stop (replace-on-offer), GtkStack
  startup warning, presence verified race-free.

## How to resume / run

- Source Rust: `. "$HOME/.cargo/env"`. Dev DB: `docker compose -f compose.dev.yml up -d postgres` (host port 5433).
- Backend: `DATABASE_URL=postgres://hearth:hearth@localhost:5433/hearth JWT_SECRET=dev-only-change-me-min-32-bytes-long-secret cargo run` (listens `0.0.0.0:8080`).
- Seed users: `password::hash` + `users::repository::create` (admin endpoint also exists).
- Engine loopback: backend up + two users, then `engine view` and `engine share` (see `engine/README.md`).
- Plans: `docs/superpowers/plans/`. Spec: `docs/superpowers/specs/2026-06-21-hearth-design.md`.
