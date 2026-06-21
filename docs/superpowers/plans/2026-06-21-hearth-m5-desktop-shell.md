# Hearth M5 – Rust GTK4 Desktop Shell Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build a pure-Rust GTK4 desktop client for Hearth – login, presence, text chat, screenshare (in-window) and voice – calling the `engine` crate directly, plus the per-flow media framework and the backend chat slice it needs.

**Architecture:** GTK4 + relm4 UI on the shared GLib main loop; the `engine` crate refactored into a library (`Session` owns the WebSocket + a `(peer, flow) → FlowPeer` registry and emits high-level `SessionEvent`s; one `webrtcbin` per media flow). Control plane (auth, presence, chat, signaling) is JSON over the existing Axum backend; media is P2P WebRTC per flow. Incoming video embeds via `gtk4paintablesink` → `gtk::Picture`, behind an engine `gtk` cargo feature so the CLI stays headless-friendly.

**Tech Stack:** Rust, GTK4 + relm4, gstreamer-rs 0.23 (`webrtcbin`, `gtk4paintablesink`, Opus/HEVC), tokio, tokio-tungstenite, reqwest (rustls), keyring, the Hearth backend (Axum + sqlx + PG18), `hearth-protocol`.

## Global Constraints

- **Work on `main`, commit locally** (committing allowed this session); one commit per completed task. Do **not** push.
- **Source Rust** with `. "$HOME/.cargo/env"` in every Bash call.
- **Postgres dev DB**: `docker compose -f compose.dev.yml up -d postgres` on host port **5433**; backend env `DATABASE_URL=postgres://hearth:hearth@localhost:5433/hearth JWT_SECRET=dev-only-change-me-min-32-bytes-long-secret`.
- **rustls only** (no system OpenSSL) for reqwest/sqlx.
- **Signaling stays a dumb relay** – the backend never parses SDP; `flow` is relayed opaquely.
- **Protocol stays dependency-light** – timestamps are `i64` unix-epoch-millis; no `time` dep in `hearth-protocol`.
- **Engine CLI must keep working** (`engine probe|share|view`) so M4 Task 5 (cross-machine) is unaffected; GTK is behind a cargo feature.
- **TDD** for the testable units (protocol, backend chat, session routing); GUI + media paths are run-and-observe with written success criteria (like M4).
- **Seed users** for runs: `cargo run --example seed_users` is gone; reseed via `password::hash` + `users::repository::create`, or reuse existing `alice`/`bob` (already in the dev DB).

---

## File Structure

```
hearth/
├── Cargo.toml                       # NEW root [workspace]
├── protocol/src/lib.rs              # + Flow, flow fields, Chat/ChatHistory/ChatEntry
├── backend/
│   ├── migrations/0002_messages.sql # NEW
│   └── src/
│       ├── chat/{mod,entity,repository}.rs   # NEW chat slice
│       └── signaling/{hub,message}.rs        # carry `flow`; handle Chat + ChatHistory
└── engine/
    ├── Cargo.toml                   # + gtk feature (gstreamer-gtk4), gstreamer-app
    └── src/
        ├── flow.rs                  # NEW: Flow re-export + VideoSink choice
        ├── peer.rs → flow_peer.rs   # FlowPeer (per-flow webrtcbin, screenshare + voice)
        ├── session.rs               # NEW: Session (WS + routing + SessionEvent)
        └── main.rs                  # CLI uses the new API
desktop/                              # NEW relm4 crate (Task 7+)
└── src/{main,app,session,config}.rs, ui/{login,room}.rs, ui/widgets/
```

---

## Task 1: Root Cargo workspace

**Files:**
- Create: `Cargo.toml` (repo root)
- Modify: remove `protocol/Cargo.lock`, `engine/Cargo.lock`, `backend/Cargo.lock` (replaced by one root lock)

**Interfaces:**
- Produces: a workspace so `engine` + the future `desktop` crate share one `target/` and lockfile.

- [ ] **Step 1: Create the root `Cargo.toml`**

```toml
[workspace]
resolver = "2"
members = ["protocol", "engine", "backend"]
# "desktop" is added in Task 7.
```

- [ ] **Step 2: Remove the per-crate lockfiles**

```bash
cd /home/maksim/Desktop/work/hearth
rm -f protocol/Cargo.lock engine/Cargo.lock backend/Cargo.lock
```

- [ ] **Step 3: Build the whole workspace**

Run: `. "$HOME/.cargo/env" && cargo build`
Expected: all three crates compile; a single root `Cargo.lock` is created.

- [ ] **Step 4: Run the workspace tests** (DB up)

Run: `. "$HOME/.cargo/env" && DATABASE_URL=postgres://hearth:hearth@localhost:5433/hearth JWT_SECRET=dev-only-change-me-min-32-bytes-long-secret cargo test`
Expected: protocol + engine + backend tests all green.

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml Cargo.lock && git rm --cached protocol/Cargo.lock engine/Cargo.lock backend/Cargo.lock 2>/dev/null; git add -A
git commit -m "build: introduce root cargo workspace"
```

---

## Task 2: Protocol – `Flow`, flow-tagged signaling, chat types

**Files:**
- Modify: `protocol/src/lib.rs`
- Modify: `backend/src/signaling/hub.rs` (carry `flow` through relay), `backend/tests/signaling.rs` (include `flow`)

**Interfaces:**
- Produces: `hearth_protocol::{Flow, ChatEntry}`; `ClientMessage`/`ServerMessage` gain `flow: Flow` on `Offer`/`Answer`/`Ice`, plus `ClientMessage::Chat{body}`, `ServerMessage::Chat{...}`, `ServerMessage::ChatHistory{messages}`.

- [ ] **Step 1: Write failing round-trip tests** (append to `protocol/src/lib.rs` tests)

```rust
    #[test]
    fn offer_carries_flow() {
        let id = Uuid::now_v7();
        let msg = ClientMessage::Offer { to: id, flow: Flow::Screen, sdp: "v=0".into() };
        let json = serde_json::to_string(&msg).unwrap();
        let back: ClientMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(msg, back);
        assert!(json.contains("\"flow\":\"screen\""));
    }

    #[test]
    fn chat_round_trips() {
        let entry = ChatEntry { from: Uuid::now_v7(), username: "alice".into(), body: "hi".into(), at: 1_700_000_000_000 };
        let msg = ServerMessage::ChatHistory { messages: vec![entry.clone()] };
        let json = serde_json::to_string(&msg).unwrap();
        let back: ServerMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(msg, back);
    }
```

- [ ] **Step 2: Run to verify failure**

Run: `cd protocol && cargo test`
Expected: FAIL – `Flow`, `ChatEntry`, `flow` field not defined.

- [ ] **Step 3: Implement in `protocol/src/lib.rs`**

```rust
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Flow { Voice, Screen, Webcam }

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChatEntry {
    pub from: Uuid,
    pub username: String,
    pub body: String,
    pub at: i64, // unix epoch millis
}
```

Add `flow: Flow` to `ClientMessage::{Offer,Answer,Ice}` and `ServerMessage::{Offer,Answer,Ice}`, and add:

```rust
// ClientMessage:
    Chat { body: String },
// ServerMessage:
    Chat { from: Uuid, username: String, body: String, at: i64 },
    ChatHistory { messages: Vec<ChatEntry> },
```

- [ ] **Step 4: Run protocol tests**

Run: `cd protocol && cargo test`
Expected: PASS (new + existing).

- [ ] **Step 5: Fix the backend signaling hub to carry `flow`**

In `backend/src/signaling/hub.rs`, every place that builds `ServerMessage::Offer/Answer/Ice` from the inbound `ClientMessage` must pass `flow` through. Example for the offer arm:

```rust
ClientMessage::Offer { to, flow, sdp } => {
    self.send_to(to, ServerMessage::Offer { from: sender, flow, sdp }).await;
}
```

(Repeat for `Answer` and `Ice`; `Ice` also carries `mline`/`candidate` as before. Leave `Chat` unhandled here for now – Task 3.)

- [ ] **Step 6: Update the existing signaling integration test** in `backend/tests/signaling.rs` to include `flow` in the offer/ice it sends and asserts (e.g. `flow: "screen"` in the JSON, or the typed `Flow::Screen`).

- [ ] **Step 7: Run the backend suite** (DB up)

Run: `cd backend && DATABASE_URL=... JWT_SECRET=... cargo test`
Expected: PASS – signaling relay still works, now with `flow`.

- [ ] **Step 8: Commit**

```bash
git add protocol backend/src/signaling backend/tests/signaling.rs Cargo.lock
git commit -m "feat(protocol): add Flow discriminator and chat message types"
```

---

## Task 3: Backend chat slice

**Files:**
- Create: `backend/migrations/0002_messages.sql`, `backend/src/chat/{mod,entity,repository}.rs`
- Modify: `backend/src/lib.rs` (register `chat` module), `backend/src/signaling/hub.rs` (handle `Chat`, send `ChatHistory` on join)
- Test: `backend/tests/chat.rs`

**Interfaces:**
- Consumes: `hearth_protocol::{ClientMessage, ServerMessage, ChatEntry}`.
- Produces: `chat::repository::{insert, recent}`; WS now persists+broadcasts chat and sends history on join.

- [ ] **Step 1: Migration** `backend/migrations/0002_messages.sql`

```sql
CREATE TABLE messages (
    id          uuid PRIMARY KEY DEFAULT uuidv7(),
    room        text NOT NULL,
    sender_user uuid NOT NULL REFERENCES users(id),
    body        text NOT NULL,
    created_at  timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX messages_room_created_idx ON messages (room, created_at);
```

- [ ] **Step 2: Entity** `backend/src/chat/entity.rs`

```rust
use time::OffsetDateTime;
use uuid::Uuid;

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Message {
    pub id: Uuid,
    pub room: String,
    pub sender_user: Uuid,
    pub body: String,
    pub created_at: OffsetDateTime,
}
```

- [ ] **Step 3: Repository** `backend/src/chat/repository.rs` (+ `mod.rs` exposing `pub mod entity; pub mod repository;`)

```rust
use super::entity::Message;
use sqlx::PgPool;
use uuid::Uuid;

pub async fn insert(pool: &PgPool, room: &str, sender: Uuid, body: &str) -> Result<Message, sqlx::Error> {
    sqlx::query_as::<_, Message>(
        "INSERT INTO messages (room, sender_user, body) VALUES ($1, $2, $3)
         RETURNING id, room, sender_user, body, created_at",
    )
    .bind(room).bind(sender).bind(body)
    .fetch_one(pool).await
}

pub async fn recent(pool: &PgPool, room: &str, limit: i64) -> Result<Vec<Message>, sqlx::Error> {
    sqlx::query_as::<_, Message>(
        "SELECT id, room, sender_user, body, created_at FROM messages
         WHERE room = $1 ORDER BY created_at DESC LIMIT $2",
    )
    .bind(room).bind(limit)
    .fetch_all(pool).await
}
```

- [ ] **Step 4: Write the failing integration test** `backend/tests/chat.rs`

Model it on `backend/tests/signaling.rs`: connect two authed WS clients to the same room; client A sends `ClientMessage::Chat { body: "hello" }`; assert client B receives `ServerMessage::Chat { body: "hello", .. }`; assert a freshly-joining client C receives `ServerMessage::ChatHistory` containing "hello". (Reuse the test harness helpers from `signaling.rs` for login + WS connect.)

- [ ] **Step 5: Run to verify failure**

Run: `cd backend && DATABASE_URL=... JWT_SECRET=... cargo test --test chat`
Expected: FAIL – chat not handled.

- [ ] **Step 6: Handle chat in the hub** (`backend/src/signaling/hub.rs`)

- On `ClientMessage::Chat { body }`: look up the sender's username + room, `chat::repository::insert(...)`, then broadcast `ServerMessage::Chat { from, username, body, at }` to the room (`at` = `created_at.unix_timestamp_nanos() / 1_000_000` as `i64`).
- On join (where `RoomPeers` is currently sent): also fetch `chat::repository::recent(pool, room, 50)`, reverse to chronological, map to `ChatEntry`, and send `ServerMessage::ChatHistory { messages }`.
- The hub needs the `PgPool`; thread it in from app state if not already present.

- [ ] **Step 7: Register the module** – add `pub mod chat;` to `backend/src/lib.rs`.

- [ ] **Step 8: Run the chat test + full suite**

Run: `cd backend && DATABASE_URL=... JWT_SECRET=... cargo test`
Expected: PASS.

- [ ] **Step 9: Commit**

```bash
git add backend Cargo.lock
git commit -m "feat(backend): chat persistence + WS relay with history on join"
```

---

## Task 4: Engine – `FlowPeer` (screenshare as a flow, configurable sink)

**Files:**
- Modify: `engine/Cargo.toml` (add `gstreamer-app`, optional `gstreamer-gtk4` behind a `gtk` feature)
- Create: `engine/src/flow.rs`
- Rename/Modify: `engine/src/peer.rs` → `engine/src/flow_peer.rs` (generalize to per-flow)
- Modify: `engine/src/lib.rs`, `engine/src/main.rs`

**Interfaces:**
- Consumes: `engine::{capture, encoders, signaling}`, `hearth_protocol::{ClientMessage, ServerMessage, Flow}`.
- Produces:
  - `engine::flow::Flow` (re-export of `hearth_protocol::Flow`).
  - `engine::flow::VideoSink` = `Auto` (autovideosink, own window) or `Paintable` (gtk4paintablesink; `gtk` feature).
  - `engine::flow_peer::FlowPeer` with `start(webrtc parent pieces…)`, internally builds capture+encode for `Flow::Screen` offerer and decode+sink for the answerer; `stop()`; reports state + (for `Paintable`) a `gdk::Paintable` via callback.

- [ ] **Step 1: Cargo features** in `engine/Cargo.toml`

```toml
gstreamer-app = "0.23"

[features]
default = []
gtk = ["dep:gstreamer-gtk4"]

[dependencies.gstreamer-gtk4]
version = "0.13"
optional = true
```

(Use the `gstreamer-gtk4` version matching gstreamer-rs 0.23; adjust if cargo reports a mismatch.)

- [ ] **Step 2: `engine/src/flow.rs`**

```rust
pub use hearth_protocol::Flow;

/// How an incoming video flow is displayed.
#[derive(Debug, Clone, Copy)]
pub enum VideoSink {
    /// autovideosink – GStreamer opens its own window (CLI / headless-ish).
    Auto,
    /// gtk4paintablesink – exposes a gdk::Paintable for in-app embedding.
    #[cfg(feature = "gtk")]
    Paintable,
}
```

- [ ] **Step 3: Generalize the peer into `flow_peer.rs`**

Move `engine/src/peer.rs` to `engine/src/flow_peer.rs`. Parameterize the existing screenshare pipeline by `Flow` and `VideoSink`:
- Sender branch for `Flow::Screen` = the current capture→encoder→h265parse→rtph265pay→caps path (unchanged from M4).
- Receiver branch: choose the sink by `VideoSink` – `Auto` builds `autovideosink` (current behavior); `Paintable` builds `gtk4paintablesink`, reads its `paintable` property, and hands it to a caller-supplied `on_paintable: impl Fn(gdk::Paintable)` callback (marshalled to the main thread via `glib::idle_add_local_once`).
- Keep the M4 bus watch, ICE buffering, and the `sender()`-clone (no `unsafe`).
- Voice (`Flow::Voice`) is added in Task 5 – leave a `todo!()`-free `match` arm that returns an explicit error for now (`anyhow::bail!("voice flow added in M5 Task 5")`), so the code compiles and screenshare works.

- [ ] **Step 4: Update `lib.rs` + `main.rs`**

- `lib.rs`: `pub mod flow; pub mod flow_peer;` (drop `pub mod peer;`).
- `main.rs`: `share`/`view` call the new `FlowPeer` with `Flow::Screen` and `VideoSink::Auto` (so the CLI is unchanged in behavior).

- [ ] **Step 5: Build (no gtk) and run the existing unit tests**

Run: `. "$HOME/.cargo/env" && cargo build -p engine && cargo test -p engine`
Expected: compiles; `encoders`/`capture`/`signaling` unit tests still pass.

- [ ] **Step 6: Build with the gtk feature**

Run: `cargo build -p engine --features gtk`
Expected: compiles (pulls `gstreamer-gtk4`). If `gtk4paintablesink` factory is missing at runtime later, install `gst-plugins-rs` GTK4 plugin – note in `engine/README.md`.

- [ ] **Step 7: Loopback regression** (backend up, users `alice`/`bob`)

Run the M4 loopback (`engine share` / `engine view`, `VideoSink::Auto`); confirm both `Connected` and the viewer still displays. This proves the refactor didn't break the screenshare flow.

- [ ] **Step 8: Commit**

```bash
git add engine Cargo.lock
git commit -m "refactor(engine): generalize peer into per-flow FlowPeer with configurable video sink"
```

---

## Task 5: Engine – voice flow (Opus)

**Files:**
- Modify: `engine/src/flow_peer.rs`, `engine/src/main.rs`

**Interfaces:**
- Produces: `FlowPeer` supports `Flow::Voice` – sender `autoaudiosrc → audioconvert → audioresample → opusenc → rtpopuspay → application/x-rtp,encoding-name=OPUS,payload=97 → webrtcbin`; receiver `rtpopusdepay → opusdec → audioconvert → autoaudiosink`. `mute(bool)` / `deafen(bool)`.

- [ ] **Step 1: Implement the voice sender branch** in `flow_peer.rs` (replace the Task 4 `bail!` arm)

```rust
// Flow::Voice sender chain (payload 97 to distinguish from video's 96):
let desc = "autoaudiosrc ! audioconvert ! audioresample ! opusenc ! rtpopuspay";
// build elements, link, caps = application/x-rtp,media=audio,encoding-name=OPUS,payload=97
```

(Mirror `build_send_branch`, but audio elements + an audio caps filter; link into the same `webrtcbin`.)

- [ ] **Step 2: Implement the voice receiver branch** – in the `pad-added` handler, branch on the caps media type: `audio` → `rtpopusdepay → opusdec → audioconvert → autoaudiosink`; `video` → the existing decode+videosink path.

- [ ] **Step 3: `mute` / `deafen`** – `mute` drops the audio send (e.g., toggle the `opusenc`/src branch state or a `valve` element on the send branch); `deafen` mutes + sets the `autoaudiosink` `volume`/drops the recv branch. Add a `valve drop=false` element on each audio branch and flip `drop` for mute/deafen.

- [ ] **Step 4: CLI `call` mode** in `main.rs` – add a `call` subcommand that runs `Flow::Voice` as offerer (mirrors `share`), for loopback testing.

- [ ] **Step 5: Build + voice loopback** (backend up)

Run `engine view` (answerer, will also accept audio) on one terminal and `engine call` on another; confirm both `Connected` and audio flows (speak into mic, hear it). Record in `engine/README.md`. (If no second audio device, at minimum confirm `Connected` + no pipeline errors.)

- [ ] **Step 6: Commit**

```bash
git add engine Cargo.lock engine/README.md
git commit -m "feat(engine): voice flow (Opus) with mute/deafen"
```

---

## Task 6: Engine – `Session` (WS + routing + `SessionEvent`)

**Files:**
- Create: `engine/src/session.rs`
- Modify: `engine/src/lib.rs`

**Interfaces:**
- Consumes: `engine::signaling::{login, SignalingClient}`, `engine::flow_peer::FlowPeer`, `hearth_protocol::*`.
- Produces:
  - `engine::session::SessionEvent` enum: `Presence(...)`, `Chat(ChatEntry)`, `ChatHistory(Vec<ChatEntry>)`, `FlowState { peer: Uuid, flow: Flow, state: String }`, `VideoReady { peer: Uuid, flow: Flow, paintable: gdk::Paintable }` (gtk feature), `Error(String)`.
  - `engine::session::Session::connect(http, ws, user, pass) -> Result<(Session, UnboundedReceiver<SessionEvent>)>`.
  - Ops: `join(room)`, `start_share(peer)`, `start_call(peer)`, `stop_flow(peer, flow)`, `mute(bool)`, `deafen(bool)`, `send_chat(body)`.

- [ ] **Step 1: Write a failing routing unit test** (mock WS, like `signaling.rs` tests) at the bottom of `session.rs`

Test that an inbound `ServerMessage::Chat` is surfaced as `SessionEvent::Chat`, and an inbound `ServerMessage::Offer { flow: Screen, .. }` creates/looks-up the screen `FlowPeer` for that peer (assert via a `SessionEvent::FlowState` or an exposed registry length). Keep it to the routing logic that does not require a live GStreamer pipeline – if pipeline construction is unavoidable, gate the media assertions behind `#[ignore]` and assert chat/presence routing only.

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p engine session::`
Expected: FAIL – `Session` undefined.

- [ ] **Step 3: Implement `Session`** – wraps `login` + `SignalingClient`; spawns a task reading inbound `ServerMessage`s and translating: presence/chat → `SessionEvent`; `Offer/Answer/Ice{from, flow}` → dispatch to the `(peer, flow) → FlowPeer` map (creating an answerer `FlowPeer` on first `Offer`); `FlowPeer` events → `SessionEvent`. Ops send the matching `ClientMessage` and/or drive a `FlowPeer`.

- [ ] **Step 4: Register + run tests**

`lib.rs`: `pub mod session;`. Run `cargo test -p engine session::` → PASS (non-ignored routing tests).

- [ ] **Step 5: Commit**

```bash
git add engine Cargo.lock
git commit -m "feat(engine): Session owns WS + per-flow routing, emits SessionEvent"
```

---

## Task 7: Desktop crate scaffold – workspace member, login → empty room

**Files:**
- Modify: root `Cargo.toml` (add `desktop` member)
- Create: `desktop/Cargo.toml`, `desktop/src/{main,app,session,config}.rs`, `desktop/src/ui/login.rs`

**Interfaces:**
- Consumes: `engine::session::{Session, SessionEvent}` (with `engine/gtk`), `keyring`, `relm4`.
- Produces: a runnable app that logs in and shows an (empty) Room.

- [ ] **Step 1: Add `desktop` to the workspace** and create `desktop/Cargo.toml`

```toml
[package]
name = "desktop"
version = "0.1.0"
edition = "2021"

[dependencies]
relm4 = "0.9"
gtk = { package = "gtk4", version = "0.9" }
engine = { path = "../engine", features = ["gtk"] }
hearth-protocol = { path = "../protocol" }
tokio = { version = "1", features = ["full"] }
keyring = "3"
anyhow = "1"
serde = { version = "1", features = ["derive"] }
toml = "0.8"
directories = "5"
uuid = { version = "1", features = ["v7", "serde"] }
```

(Pin `relm4`/`gtk4` to the versions cargo resolves against gstreamer-gtk4 0.13; adjust if needed.)

- [ ] **Step 2: `config.rs`** – server URLs from a TOML in `directories::ProjectDirs` config dir (defaults `http://127.0.0.1:8080` / `ws://127.0.0.1:8080`, env override `HEARTH_HTTP`/`HEARTH_WS`); `keyring` token get/set with a file fallback under the config dir + `HEARTH_TOKEN` env.

- [ ] **Step 3: `session.rs`** – a thin relm4 worker/async-command wrapper that owns `engine::Session` and forwards `SessionEvent`s as relm4 messages to the root component.

- [ ] **Step 4: `ui/login.rs`** – a relm4 Component: username/password fields + Login button; on submit calls `Session::connect`, stores the token, and emits `LoggedIn(Session, receiver)` to the root.

- [ ] **Step 5: `app.rs` + `main.rs`** – root Component routes `Login ⇄ Room`; `main.rs` does `gtk::init`/relm4 boot, `gst` init via engine, restores a token (skip to Room if present), else Login. Room is a placeholder showing "connected as <user>" for this task.

- [ ] **Step 6: Build**

Run: `. "$HOME/.cargo/env" && cargo build -p desktop`
Expected: compiles. (First GTK build pulls many deps – allow time.)

- [ ] **Step 7: Run-and-observe** (backend up, user `alice`)

Run: `HEARTH_USER=… ` not needed – type `alice` / `pw-alice` in the form. `cargo run -p desktop`.
**Success:** the login window appears; logging in switches to the Room placeholder showing the logged-in user. Record in `desktop/README.md`.

- [ ] **Step 8: Commit**

```bash
git add desktop Cargo.toml Cargo.lock
git commit -m "feat(desktop): relm4 scaffold – login to empty room"
```

---

## Task 8: Desktop Room – presence list + chat panel

**Files:**
- Create: `desktop/src/ui/room.rs`, `desktop/src/ui/widgets/{peer_row,message_bubble}.rs`
- Modify: `desktop/src/app.rs`

**Interfaces:**
- Consumes: `SessionEvent::{Presence, Chat, ChatHistory}`, `Session::{join, send_chat}`.
- Produces: a Room component showing online peers and a working chat panel.

- [ ] **Step 1: Room component** – on entry, `Session::join("main")`; render a presence list (left) from `Presence` events and a chat panel (center) from `ChatHistory` + `Chat` events; a text entry + Send → `Session::send_chat(body)`.
- [ ] **Step 2: `peer_row` / `message_bubble`** factory widgets.
- [ ] **Step 3: Build** – `cargo build -p desktop`.
- [ ] **Step 4: Run-and-observe** with two app instances (alice + bob, set distinct config dirs or run on two users): each sees the other in presence; a message from alice appears for bob; a newly-joined client shows history. Record in `desktop/README.md`.
- [ ] **Step 5: Commit**

```bash
git add desktop Cargo.lock && git commit -m "feat(desktop): room presence list + chat panel"
```

---

## Task 9: Desktop Room – video, share/view, voice controls

**Files:**
- Modify: `desktop/src/ui/room.rs`, `desktop/src/app.rs`

**Interfaces:**
- Consumes: `SessionEvent::{FlowState, VideoReady}`, `Session::{start_share, start_call, stop_flow, mute, deafen}`.
- Produces: in-window video + media controls; the full M5 success criteria.

- [ ] **Step 1: Video area** – a `gtk::Picture`; on `SessionEvent::VideoReady { paintable, .. }` call `picture.set_paintable(Some(&paintable))`.
- [ ] **Step 2: Controls** – per-peer "Share" (`start_share`) and "Call" (`start_call`); a "Stop" (`stop_flow`); mic **Mute** (`mute`) and **Deafen** (`deafen`) toggles; a connection-state chip fed by `FlowState`.
- [ ] **Step 3: Build** – `cargo build -p desktop`.
- [ ] **Step 4: Run-and-observe – full M5 criteria** (backend up, alice + bob):
  - log in (both), see presence, exchange chat;
  - alice clicks Share → bob sees alice's screen **in-window**;
  - start a call → voice connects (hear audio); Mute/Deafen work;
  - alice stops screenshare → **voice + chat keep running** (per-flow independence).
  Record results in `desktop/README.md`.
- [ ] **Step 5: Commit**

```bash
git add desktop Cargo.lock && git commit -m "feat(desktop): in-window video, share/view, voice controls"
```

---

## Self-Review (completed during authoring)

- **Spec coverage:** workspace (T1), protocol Flow+chat (T2), backend chat slice (T3), engine per-flow refactor + `gtk4paintablesink` sink (T4), voice flow + mute/deafen (T5), `Session`/`SessionEvent`/routing (T6), desktop login+config+keyring (T7), presence+chat UI (T8), video+share+voice UI (T9). Concurrency/isolation is structural (per-flow `webrtcbin` + tokio + GLib loop) and exercised in T9. Out-of-scope items (webcam, mesh, theming, packaging, coturn) are intentionally absent.
- **Placeholder scan:** TDD code is concrete for the testable units (T2/T3/T6); GUI + media tasks (T4/T5/T7–T9) are run-and-observe with explicit success criteria, matching the spec's testing section and the M4 precedent. No "TBD".
- **Type consistency:** `Flow`, `ChatEntry`, `flow` fields, `SessionEvent` variants, and `Session` op names (`start_share`/`start_call`/`stop_flow`/`mute`/`deafen`/`send_chat`) match between the protocol, engine, and desktop tasks.
- **Risk note:** version pins for `gstreamer-gtk4` ↔ `relm4`/`gtk4` are the most likely adjustment (T4 Step 1, T7 Step 1); `gtk4paintablesink` plugin presence (T4 Step 6) and a Secret Service at dev time (T7 keyring fallback) are called out.
