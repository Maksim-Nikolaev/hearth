# Hearth M5 – Rust GTK4 Desktop Shell (Design)

_Status: design agreed 2026-06-21. Supersedes the earlier "Flutter + flutter_rust_bridge" plan for the desktop client and the "Approach A bundle" media transport._

## Goal

Turn Hearth's CLI engine into a usable **desktop application**: log in, see who's
online, exchange text chat, and **share / view a screen** (with in-window video)
and **talk over voice** – all against the existing Rust/Axum backend. The app is
written in **pure Rust** (GTK4 + relm4) and calls the `engine` crate directly, so
there is no language bridge.

This milestone also lays the **per-flow media framework** that voice and
screenshare use now, and that webcam and multi-peer mesh slot into later.

## Locked decisions

- **Desktop UI: GTK4 + relm4, pure Rust, no flutter_rust_bridge / no Tauri.** GTK
  and GStreamer both run on the GLib main loop, so the engine needs no separate
  loop or thread, and incoming video embeds in-window almost for free via
  `gtk4paintablesink` → `gtk::Picture`.
- **Flutter is a later, separate mobile app** – shares only the backend +
  protocol, uses `flutter_webrtc`, not this engine. Out of scope here.
- **Media transport: per-flow PeerConnections** – one `webrtcbin` per flow.
  Chat over the WebSocket. Each flow connects, drops, and congests independently.
- **Cargo workspace** at the repo root (`protocol`, `engine`, `backend`,
  `desktop`) – shared lockfile and `target/` so the engine + GStreamer + GTK
  stack compiles once.
- **Token storage: `keyring` crate** (OS secure vault) with a file/env dev
  fallback when no Secret Service is available.
- **In M5:** login, presence, chat (+ backend chat slice), screenshare flow
  (share/view, in-window), **voice flow** (mute/deafen), connection-state UI,
  token persistence, the per-flow framework + flow-tagged signaling.
- **Out of M5 (next):** webcam flow, multi-peer mesh, custom theming,
  packaging / auto-update, TURN/coturn (M6 = Traefik proxy + coturn relay).

## Architecture

```
┌─────────────────────────── desktop (relm4, GTK4) ───────────────────────────┐
│  Login ──► Room (presence | chat | video gtk::Picture | share/view | voice)  │
│                              │ relm4 messages                                │
│                       session.rs (bridge)                                    │
└──────────────────────────────┼──────────────────────────────────────────────┘
                               │ direct calls + event receiver
┌──────────────────────────────▼──────────── engine (library) ────────────────┐
│  Session  ── login (REST) + one WebSocket (control plane) ──┐                 │
│     │ routes inbound by (peer, flow)                        │ tokio thread    │
│     ├──► FlowPeer(Voice)        webrtcbin  (Opus)           │                 │
│     ├──► FlowPeer(Screen)       webrtcbin  (HEVC + audio)   │ GLib/GTK thread │
│     └──► FlowPeer(Webcam)*      webrtcbin  (video)  *later  │                 │
└──────────────────────────────┬──────────────────────────────────────────────┘
                               │ HTTP + WS (JSON: hearth-protocol)
┌──────────────────────────────▼──────────── backend (Axum) ──────────────────┐
│  /auth/*   ·   /ws  (presence + signaling + chat)   ·   messages (Postgres)  │
└──────────────────────────────────────────────────────────────────────────────┘
                               │ P2P WebRTC (per flow), never via backend
                          other peer's engine
```

**Threading model.** The GTK main thread runs the GLib main loop; all GStreamer
pipelines and `webrtcbin` callbacks live on it. The control-plane WebSocket and
REST login run on a **tokio runtime on a background thread**; inbound server
messages cross to the UI as relm4 messages. Nothing blocks the UI thread:
encoding runs on GStreamer streaming threads, network on tokio. (See
"Concurrency & isolation".)

**One WebSocket, three concerns.** The single `/ws` connection carries presence,
signaling, and chat. `Session` owns it and fans inbound messages out: presence +
chat → UI, signaling → the matching `FlowPeer`.

## Components

### 1. `hearth-protocol` (extend)

Add the flow discriminator and chat. Both enums keep `Serialize + Deserialize`.

```rust
pub enum Flow { Voice, Screen, Webcam }   // serde snake_case

// ClientMessage gains `flow` on the media variants, and a chat variant:
Offer  { to: Uuid, flow: Flow, sdp: String }
Answer { to: Uuid, flow: Flow, sdp: String }
Ice    { to: Uuid, flow: Flow, mline: u32, candidate: String }
Chat   { body: String }

// ServerMessage mirror:
Offer  { from: Uuid, flow: Flow, sdp: String }
Answer { from: Uuid, flow: Flow, sdp: String }
Ice    { from: Uuid, flow: Flow, mline: u32, candidate: String }
Chat        { from: Uuid, username: String, body: String, at: i64 }  // at = unix epoch millis
ChatHistory { messages: Vec<ChatEntry> }   // sent on join

pub struct ChatEntry { pub from: Uuid, pub username: String, pub body: String, pub at: i64 }
```

`at` is an `i64` of unix epoch milliseconds so the protocol crate stays
dependency-light (no `time` dep); the UI/back end convert as needed. `flow` lets
a peer run several `webrtcbin`s with one signaling channel; the backend relays it
opaquely (still a dumb relay – it never parses SDP).

### 2. Backend chat slice

- **Migration + table** `messages` (uuidv7 id, room, sender_user FK, body,
  created_at). Mirrors the existing migration/repo style.
- **Repo**: `insert(room, sender, body) -> ChatEntry`, `recent(room, limit)`.
- **WS handler**: on `ClientMessage::Chat`, persist then broadcast
  `ServerMessage::Chat` to the room (reusing the existing room/hub). On join,
  send `ChatHistory` with the recent N. Signaling `Offer/Answer/Ice` now carry
  `flow` and are relayed unchanged.
- **Test**: an integration test like the current `signaling.rs` – two clients,
  one sends chat, the other receives it; history is delivered on join.

### 3. `engine` library refactor

The CLI logic becomes a reusable API; `main.rs` keeps `probe`/`share`/`view`
working (so M4 Task 5 is unaffected).

- **`Session`** – owns the WebSocket *and* the `(peer, flow) → FlowPeer` registry,
  so the UI never touches raw signaling.
  - `connect(http, ws, user, pass) -> Result<(Session, Receiver<SessionEvent>)>`
    – REST login + open WS; returns the handle and a stream of **high-level**
    events.
  - High-level ops: `start_share(peer)`, `start_call(peer)` (voice),
    `stop_flow(peer, flow)`, `mute(bool)`, `deafen(bool)`, `send_chat(body)`.
  - Internally: inbound `Offer/Answer/Ice{from, flow}` are dispatched to the
    matching `FlowPeer` (created on demand); presence + chat are forwarded as
    events.
  - **`SessionEvent`** (what the UI consumes): `Presence(RoomPeers/PeerJoined/
    PeerLeft)`, `Chat(ChatEntry)`, `ChatHistory(Vec<ChatEntry>)`,
    `FlowState { peer, flow, state }`, `VideoReady { peer, flow, paintable }`,
    `Error(String)`.
- **`Flow { Voice, Screen, Webcam }`** – shared with the protocol.
- **`FlowPeer`** – wraps one `webrtcbin` for a `(peer, flow, role)`:
  - `start(...)` builds the pipeline: offerer captures+encodes+sends; answerer
    receives. Screenshare uses the M4 capture/encoder path (now a flow); voice
    uses `autoaudiosrc → opusenc → rtpopuspay` out and
    `rtpopusdepay → opusdec → autoaudiosink` in.
  - **Video sink** = `gtk4paintablesink`; exposes its `gdk::Paintable` for the
    UI. The decode branch is built in a `pad-added` callback on a GStreamer
    streaming thread, so the paintable is handed to the UI by **marshalling to
    the GTK main thread** (GLib idle / relm4 sender).
  - `mute(bool)` / `deafen(bool)` for the voice flow (gate send / playback).
  - `stop()` tears down just this flow.
  - Emits events (connection-state, stream-ready, error) up to `Session`, which
    forwards them as `SessionEvent`s – no `println!`.

Per-OS capture/devices reuse the M4 env-override pattern (`HEARTH_CAPTURE`, etc.);
audio source/sink default to `autoaudiosrc`/`autoaudiosink` with room to pin a
device later.

### 4. `desktop` crate (relm4)

```
desktop/src/
  main.rs        gst::init, load config, restore token (keyring), launch app
  app.rs         root Component: routes Login <-> Room, owns Session
  session.rs     bridges engine Session events -> relm4 messages (async cmd)
  config.rs      server URLs (config file + env), keyring token store + fallback
  ui/login.rs    username/password form -> Session::connect
  ui/room.rs     presence list | chat panel | video (gtk::Picture) |
                 share/view controls | mic mute / deafen | connection-state chip
  ui/widgets/    message bubble, peer row
```

- **Structure** mirrors Lezio's feature-first spirit; relm4 Components replace
  Riverpod providers (Lezio uses hand-written providers – we mirror that
  simplicity, no codegen).
- **Token storage**: `keyring` (Secret Service / Credential Manager / Keychain);
  if unavailable (headless dev), fall back to a file in the config dir or a
  `HEARTH_TOKEN` env var.
- **Config**: server base URLs from a small TOML in the platform config dir,
  overridable by env. Minimal GTK CSS; distinctive theming deferred.
- **Video mounting**: `gtk::Picture::new()` whose paintable is set from the
  `FlowPeer`'s `gdk::Paintable` once the inbound stream is ready.

## Data flow (happy paths)

- **Launch**: restore token → if valid, go to Room; else Login.
- **Login**: form → `Session::connect` → store token (keyring) → Room.
- **Join room**: `Session` sends `Join`; receives `RoomPeers` + `ChatHistory`;
  UI renders presence + history.
- **Chat**: type → `ClientMessage::Chat` → backend persists + broadcasts →
  `ServerMessage::Chat` → appended to the panel.
- **Start a call (voice)**: "Call" on a peer → `Session::start_call(peer)` →
  voice `FlowPeer` (offerer) → offer/answer/ICE tagged `flow: voice` → connected
  → `FlowState` event flips the UI to in-call; mute/deafen gate audio.
- **Start screenshare**: "Share" on a peer → `Session::start_share(peer)` →
  screen `FlowPeer` (offerer), tagged `flow: screen` → remote side's `pad-added`
  builds decode → `VideoReady { paintable }` → mounted in `gtk::Picture`.
- **Drop a flow**: `Session::stop_flow(peer, flow)` tears that `webrtcbin` down;
  other flows and chat are unaffected.

## Concurrency & isolation

- GStreamer runs each pipeline branch on its own streaming threads – screenshare
  HEVC encode cannot stall voice (separate flow, separate threads, separate
  PeerConnection).
- tokio owns the WS with split non-blocking inbound/outbound tasks (no
  head-of-line blocking); the GTK main loop only handles lightweight messages.
- Per-flow transports mean congestion is isolated: a screenshare burst can't
  starve voice, because they're different ICE/DTLS transports.

## Error handling

- Engine surfaces GStreamer bus errors/warnings (the M4 bus watch) and
  connection-state changes as events; the UI shows a status chip and
  non-fatal error toasts rather than panicking.
- Login/network failures map to a visible message on the Login screen.
- A failed flow stops just that flow and reports it; the session and other flows
  stay up. WS disconnect surfaces a "reconnecting" state (basic reconnect).

## Testing

- **Unit/integration (Rust):** protocol round-trips incl. `flow` + chat; backend
  chat repo + WS relay/history integration test; engine routing logic via the
  existing mock-WS pattern.
- **Run-and-observe (like M4):** launch the app against the backend with two
  users; success criteria: log in, see presence, exchange chat, start
  screenshare and see it in-window, hear voice, and verify stopping screenshare
  leaves voice + chat running. Recorded in `desktop/README.md`.

## Scope / non-goals

In: as listed under "Locked decisions". Explicitly **not** in M5: webcam flow,
multi-peer mesh (1:1 media now, full presence), custom theme, installers /
auto-update, TURN/coturn deployment.

## Risks & open implementation questions

- **`gtk4paintablesink` availability** – ships in `gst-plugins-rs`; must be
  installed/bundled. Verify it's present on the dev box early; it's the one new
  hard dependency.
- **keyring on Linux dev** – no Secret Service on a headless box; the file/env
  fallback covers it.
- **Per-OS audio devices** – `autoaudiosrc`/`autoaudiosink` should "just work" on
  the X11 dev box; device selection is a later refinement.
- **Marshalling the paintable** off the streaming thread to the GTK thread must
  be correct (GLib idle) or the video won't mount – call it out in the plan.
- **Workspace migration** – folding `backend` (own lockfile today) into the root
  workspace; low risk, verify all crates still build/test after.
