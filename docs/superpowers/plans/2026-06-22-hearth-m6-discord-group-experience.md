# Hearth M6 – Discord-style Group Experience Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Turn the 1:1 M5 desktop shell into a Discord-style group app: a 3-pane layout, a Voice channel with group voice over a P2P mesh, and multi-sharer screenshare with an instant-switch stage — architected so a backend SFU can replace screenshare fan-out later.

**Architecture:** Backend gains a voice sub-room (membership + relay). The engine `Session` opens a Voice `FlowPeer` to every voice member (mesh; smaller-UUID peer offers) and a Screen `FlowPeer` to each viewer when sharing (behind a `ScreenTransport` seam, P2P now). The desktop monolith splits into relm4 sub-components (channels, self-panel, stage, chat, members) under a workspace shell, with a dark CSS theme.

**Tech Stack:** Rust, GTK4 + relm4 (`FactoryVecDeque`), gstreamer-rs 0.23 (`webrtcbin`, Opus/HEVC, `gtk4paintablesink`), tokio, the Hearth backend (Axum + sqlx), `hearth-protocol`.

## Global Constraints

- **Work on `main`, commit locally** (committing allowed); one commit per task. Do **not** push.
- **Source Rust** with `. "$HOME/.cargo/env"` in every Bash call.
- **Postgres dev DB** up: `docker compose -f compose.dev.yml up -d postgres` (host port 5433); backend env `DATABASE_URL=postgres://hearth:hearth@localhost:5433/hearth JWT_SECRET=dev-only-change-me-min-32-bytes-long-secret`.
- **Signaling stays a dumb relay** — backend never parses SDP; voice/screenshare media is P2P and never transits the backend (until the future SFU).
- **Voice = P2P mesh always. Screenshare = P2P now, behind a `ScreenTransport` seam for a future SFU.**
- **Glare rule (voice only):** the peer with the smaller `Uuid` is the offerer; screenshare is always sharer→viewer.
- **TDD** for protocol/backend/engine logic; UI + media are run-and-observe with written success criteria.
- **Multi-instance testing:** distinct `HEARTH_TITLE` per instance (sets window title + a distinct GtkApplication id). Mint tokens via `/auth/login` (users `alice`/`bob` exist, `pw-alice`/`pw-bob`); pass `HEARTH_TOKEN` to auto-connect. In-window video needs `gtk4paintablesink` — the app auto-adds `~/.local/lib/hearth-gst-plugins` to `GST_PLUGIN_PATH`.

---

## File Structure

```
hearth/
├── protocol/src/lib.rs              # + voice/share message variants
├── backend/src/
│   ├── signaling/hub.rs             # + voice membership + share/voice relay
│   └── presence/ws.rs               # dispatch the new ClientMessages
└── engine/src/session.rs            # voice roster, should_offer, group voice,
│                                    #   start/stop_share, ScreenTransport, events
└── desktop/src/
    ├── app.rs                       # root shell: Session + Login<->Workspace
    ├── theme.rs                     # NEW dark GTK CSS
    └── ui/
        ├── login.rs                 # NEW (extracted)
        ├── workspace.rs             # NEW 3-pane container
        ├── channels.rs              # NEW text + voice channel list
        ├── self_panel.rs            # NEW name + mute/deafen/share
        ├── stage.rs                 # NEW gtk::Picture + Watching switcher
        ├── chat.rs                  # NEW messages (FactoryVecDeque) + entry
        └── members.rs               # NEW In Voice / Online / Offline
```

---

## Task 1: Protocol — voice + share messages

**Files:** Modify `protocol/src/lib.rs`; fix exhaustive matches in `backend/src/presence/ws.rs` and `engine/src/session.rs`, `engine/src/flow_peer.rs`.

**Interfaces:**
- Produces: `ClientMessage::{VoiceJoin, VoiceLeave, ShareStart, ShareStop}` and `ServerMessage::{VoiceState{members:Vec<PeerInfo>}, VoiceJoined{user,username}, VoiceLeft{user}, ShareStarted{user}, ShareStopped{user}}`.

- [ ] **Step 1: Write failing round-trip tests** (append to `protocol/src/lib.rs` `tests`)

```rust
    #[test]
    fn voice_and_share_round_trip() {
        let id = Uuid::now_v7();
        for msg in [
            ServerMessage::VoiceJoined { user: id, username: "a".into() },
            ServerMessage::VoiceLeft { user: id },
            ServerMessage::ShareStarted { user: id },
            ServerMessage::ShareStopped { user: id },
        ] {
            let j = serde_json::to_string(&msg).unwrap();
            assert_eq!(msg, serde_json::from_str::<ServerMessage>(&j).unwrap());
        }
        let cm = ClientMessage::VoiceJoin;
        assert_eq!(cm, serde_json::from_str::<ClientMessage>(&serde_json::to_string(&cm).unwrap()).unwrap());
    }
```

- [ ] **Step 2: Run to verify failure** — `cd protocol && cargo test` → FAIL (variants undefined).

- [ ] **Step 3: Add the variants** to `protocol/src/lib.rs`

```rust
// in ClientMessage:
    VoiceJoin,
    VoiceLeave,
    ShareStart,
    ShareStop,
// in ServerMessage:
    VoiceState { members: Vec<PeerInfo> },
    VoiceJoined { user: Uuid, username: String },
    VoiceLeft { user: Uuid },
    ShareStarted { user: Uuid },
    ShareStopped { user: Uuid },
```

- [ ] **Step 4: Run protocol tests** — `cd protocol && cargo test` → PASS.

- [ ] **Step 5: Fix exhaustive matches** so the workspace compiles:
  - `backend/src/presence/ws.rs` `dispatch`: add arms `ClientMessage::VoiceJoin => {}` … (real handling in Task 2; no-ops keep it compiling).
  - `engine/src/session.rs` `handle`: add `ServerMessage::VoiceState{..} | VoiceJoined{..} | VoiceLeft{..} | ShareStarted{..} | ShareStopped{..} => {}` (real handling in Task 3).
  - `engine/src/flow_peer.rs` `handle_signal`: add the same `ServerMessage` arms as a no-op alongside the existing `Chat | ChatHistory` arm.

- [ ] **Step 6: Build the workspace** — `cargo build` → compiles.

- [ ] **Step 7: Commit**

```bash
git add protocol backend/src/presence/ws.rs engine/src Cargo.lock
git commit -m "feat(protocol): voice channel + screenshare signaling messages"
```

---

## Task 2: Backend — voice sub-room + share relay

**Files:** Modify `backend/src/signaling/hub.rs`, `backend/src/presence/ws.rs`; Test `backend/tests/voice.rs`.

**Interfaces:**
- Produces on the hub: `voice_join(user)`, `voice_leave(user)`, `share_start(user)`, `share_stop(user)` — broadcasting the matching `ServerMessage`s to voice members. `disconnect` also voice-leaves.

- [ ] **Step 1: Add voice state to the hub** (`SignalingHub`) — a `voice: Arc<Mutex<HashSet<Uuid>>>` field (init in `Default`).

- [ ] **Step 2: Write the failing integration test** `backend/tests/voice.rs` (model on `tests/chat.rs`): two authed WS clients; A sends `{"type":"voice_join"}`, gets `voice_state` (members empty); B sends `voice_join`, A receives `voice_joined{user:B}` and B receives `voice_state{members:[A]}`; A sends `{"type":"share_start"}`, B receives `share_started{user:A}`; A disconnects, B receives `voice_left{user:A}`.

- [ ] **Step 3: Run to verify failure** — `cd backend && DATABASE_URL=… JWT_SECRET=… cargo test --test voice` → FAIL.

- [ ] **Step 4: Implement hub methods** (`hub.rs`)

```rust
pub fn voice_join(&self, user: Uuid) {
    let peers = self.peers.lock().unwrap();
    let mut voice = self.voice.lock().unwrap();

    let username = peers.get(&user).map(|p| p.username.clone()).unwrap_or_default();
    let members: Vec<PeerInfo> = voice.iter()
        .filter_map(|id| peers.get(id).map(|p| PeerInfo { user: *id, username: p.username.clone() }))
        .collect();

    for id in voice.iter() {
        if let Some(p) = peers.get(id) {
            let _ = p.tx.send(ServerMessage::VoiceJoined { user, username: username.clone() });
        }
    }
    if let Some(p) = peers.get(&user) {
        let _ = p.tx.send(ServerMessage::VoiceState { members });
    }
    voice.insert(user);
}

pub fn voice_leave(&self, user: Uuid) {
    let peers = self.peers.lock().unwrap();
    let mut voice = self.voice.lock().unwrap();
    if !voice.remove(&user) { return; }
    for id in voice.iter() {
        if let Some(p) = peers.get(id) {
            let _ = p.tx.send(ServerMessage::VoiceLeft { user });
        }
    }
}

fn voice_broadcast(&self, msg: ServerMessage) {
    let peers = self.peers.lock().unwrap();
    let voice = self.voice.lock().unwrap();
    for id in voice.iter() {
        if let Some(p) = peers.get(id) { let _ = p.tx.send(msg.clone()); }
    }
}

pub fn share_start(&self, user: Uuid) { self.voice_broadcast(ServerMessage::ShareStarted { user }); }
pub fn share_stop(&self, user: Uuid) { self.voice_broadcast(ServerMessage::ShareStopped { user }); }
```

  In `disconnect`, call `self.voice_leave(user)` before removing the peer.

- [ ] **Step 5: Dispatch the messages** (`ws.rs` `dispatch`)

```rust
        ClientMessage::VoiceJoin => state.signaling.voice_join(from),
        ClientMessage::VoiceLeave => state.signaling.voice_leave(from),
        ClientMessage::ShareStart => state.signaling.share_start(from),
        ClientMessage::ShareStop => state.signaling.share_stop(from),
```

- [ ] **Step 6: Run the voice test + full backend suite** — `cd backend && DATABASE_URL=… JWT_SECRET=… cargo test` → PASS.

- [ ] **Step 7: Commit**

```bash
git add backend Cargo.lock
git commit -m "feat(backend): voice channel membership + share relay"
```

---

## Task 3: Engine — offerer rule + group voice

**Files:** Modify `engine/src/session.rs`.

**Interfaces:**
- Produces on `Session`: `self_id: Uuid` field (the logged-in user), `join_voice()`, `leave_voice()`, and event surfacing. New `SessionEvent` variants: `VoiceState(Vec<PeerInfo>)`, `VoiceJoined{user,username}`, `VoiceLeft{user}`, `ShareStarted{user}`, `ShareStopped{user}`.
- Free fn `pub(crate) fn should_offer(me: Uuid, peer: Uuid) -> bool { me < peer }`.

- [ ] **Step 1: Write failing unit tests** (in `session.rs` `tests`)

```rust
    #[test]
    fn smaller_uuid_offers() {
        let a = Uuid::from_u128(1);
        let b = Uuid::from_u128(2);
        assert!(should_offer(a, b));
        assert!(!should_offer(b, a));
    }

    #[test]
    fn voice_state_is_surfaced() {
        let (mut s, mut rx) = Session::for_test();
        s.handle(ServerMessage::VoiceState { members: vec![] });
        assert!(matches!(rx.try_recv().unwrap(), SessionEvent::VoiceState(m) if m.is_empty()));
    }
```

  (`Session::for_test` must set a `self_id`; give it `Uuid::nil()` there.)

- [ ] **Step 2: Run to verify failure** — `cargo test -p engine session::` → FAIL.

- [ ] **Step 3: Implement.** Add `self_id: Uuid` to `Session` (decode it from the JWT in `open`/`start`, or pass the user id through `Connection`; simplest: `Connection` already has the token — add `user_id` parsed from the login response `/auth/me` is overkill, so include the id in the access-token claims read at `start`. If unavailable, derive from the first `VoiceState`/presence that names self — but prefer the token. Store it).

```rust
pub fn join_voice(&self) { let _ = self.out_tx.send(ClientMessage::VoiceJoin); }
pub fn leave_voice(&mut self) {
    let voice_peers: Vec<Uuid> = self.peers.keys().filter(|(_, f)| *f == Flow::Voice).map(|(p, _)| *p).collect();
    for p in voice_peers { self.stop_flow(p, Flow::Voice); }
    let _ = self.out_tx.send(ClientMessage::VoiceLeave);
}

fn connect_voice(&mut self, peer: Uuid) {
    if peer == self.self_id || self.peers.contains_key(&(peer, Flow::Voice)) { return; }
    if should_offer(self.self_id, peer) {
        if let Ok(fp) = FlowPeer::new(Flow::Voice, Role::Offerer, peer, self.sink, self.out_tx.clone(), self.evt_tx.clone()) {
            self.peers.insert((peer, Flow::Voice), fp);
        }
    }
    // else: wait for their offer (handle() creates an answerer on receipt)
}
```

  In `handle`, add:

```rust
            ServerMessage::VoiceState { members } => {
                for m in &members { self.connect_voice(m.user); }
                self.emit(SessionEvent::VoiceState(members));
            }
            ServerMessage::VoiceJoined { user, username } => {
                self.connect_voice(user);
                self.emit(SessionEvent::VoiceJoined { user, username });
            }
            ServerMessage::VoiceLeft { user } => {
                self.stop_flow(user, Flow::Voice);
                self.emit(SessionEvent::VoiceLeft { user });
            }
            ServerMessage::ShareStarted { user } => self.emit(SessionEvent::ShareStarted { user }),
            ServerMessage::ShareStopped { user } => self.emit(SessionEvent::ShareStopped { user }),
```

- [ ] **Step 4: Register events + run tests** — add the `SessionEvent` variants; `cargo test -p engine session::` → PASS.

- [ ] **Step 5: Commit**

```bash
git add engine Cargo.lock
git commit -m "feat(engine): group voice mesh with deterministic offerer rule"
```

---

## Task 4: Engine — multi-share + ScreenTransport seam

**Files:** Modify `engine/src/session.rs`.

**Interfaces:**
- Produces on `Session`: `start_share()` (no arg — shares to all voice members) and `stop_share()`; a `trait ScreenTransport` with `struct P2pTransport`. `sharers()` is derived by the UI from `ShareStarted/Stopped` events, not the engine.

- [ ] **Step 1: Define the seam** (top of `session.rs`)

```rust
/// How a local screenshare reaches viewers. P2P now; an SFU impl replaces this
/// later without changing the UI.
pub trait ScreenTransport {
    /// Begin sharing to the given current voice members.
    fn start(&mut self, session: &mut Session, viewers: &[Uuid]);
    fn stop(&mut self, session: &mut Session);
}

pub struct P2pTransport;
impl ScreenTransport for P2pTransport {
    fn start(&mut self, session: &mut Session, viewers: &[Uuid]) {
        for &v in viewers { let _ = session.start_offerer(v, Flow::Screen); }
    }
    fn stop(&mut self, session: &mut Session) {
        let screens: Vec<Uuid> = session.peers.keys()
            .filter(|(_, f)| *f == Flow::Screen).map(|(p, _)| *p).collect();
        for p in screens { session.stop_flow(p, Flow::Screen); }
    }
}
```

- [ ] **Step 2: Add share ops** to `Session` (hold a `screen_transport: Box<dyn ScreenTransport>` defaulting to `P2pTransport`, and a `voice_members: Vec<Uuid>` kept in sync in `connect_voice`/`VoiceLeft`)

```rust
pub fn start_share(&mut self) {
    let _ = self.out_tx.send(ClientMessage::ShareStart);
    let viewers = self.voice_members.clone();
    let mut t = std::mem::replace(&mut self.screen_transport, Box::new(P2pTransport));
    t.start(self, &viewers);
    self.screen_transport = t;
}
pub fn stop_share(&mut self) {
    let _ = self.out_tx.send(ClientMessage::ShareStop);
    let mut t = std::mem::replace(&mut self.screen_transport, Box::new(P2pTransport));
    t.stop(self);
    self.screen_transport = t;
}
```

  (The `mem::replace` dance lets the transport borrow `&mut Session` without aliasing the boxed field. Make `start_offerer`, `stop_flow`, and `peers` `pub(crate)` so the transport can call them.)

- [ ] **Step 3: Keep `voice_members` in sync** — push in `connect_voice` and on `VoiceState`; remove on `VoiceLeft`; clear in `leave_voice`.

- [ ] **Step 4: Build + unit tests** — `cargo build -p engine && cargo test -p engine` → PASS (existing tests; the share path is exercised run-and-observe in Task 7).

- [ ] **Step 5: Commit**

```bash
git add engine Cargo.lock
git commit -m "feat(engine): multi-viewer screenshare behind a ScreenTransport seam"
```

---

## Task 5: Desktop — workspace shell + theme (component split foundation)

**Files:** Modify `desktop/src/app.rs`, `desktop/src/main.rs`; Create `desktop/src/theme.rs`, `desktop/src/ui/mod.rs`, `desktop/src/ui/login.rs`, `desktop/src/ui/workspace.rs`.

**Interfaces:**
- Produces: a `Workspace` relm4 component holding the 3-pane `gtk::Box` skeleton (empty channels | stage placeholder | members), shown after login; a dark CSS sheet loaded at startup.

- [ ] **Step 1: `theme.rs`** — a `pub fn load()` that installs a `gtk::CssProvider` (dark background `#2b2d31`, sidebars `#1e1f22`, accent `#5865f2`, light text) on the default display.

- [ ] **Step 2: Extract login** into `ui/login.rs` as a relm4 component (form → emits `Login{username,password}` to the parent), replacing the inline login page.

- [ ] **Step 3: `ui/workspace.rs`** — a relm4 component with the 3-pane `gtk::Box` (left rail `#1e1f22` width 220 with a bottom self-panel slot; center stage+chat; right members width 220). For this task the panes are placeholders (labels).

- [ ] **Step 4: `app.rs`** — root shell owns `Session`, routes `Login <-> Workspace` (the `Stack` from M5, now with a `workspace` page hosting the `Workspace` controller). `main.rs` calls `theme::load()` after `RelmApp::new`.

- [ ] **Step 5: Build** — `cargo build -p desktop` → compiles.

- [ ] **Step 6: Run-and-observe** — `HEARTH_TOKEN=<alice> cargo run -p desktop`; **success:** dark 3-pane window appears after auto-connect. Record in `desktop/README.md`.

- [ ] **Step 7: Commit**

```bash
git add desktop Cargo.lock
git commit -m "feat(desktop): dark workspace shell + login extraction"
```

---

## Task 6: Desktop — members, channels, self-panel

**Files:** Create `desktop/src/ui/{members,channels,self_panel}.rs`; Modify `desktop/src/ui/workspace.rs`, `desktop/src/app.rs`.

**Interfaces:**
- Consumes: `SessionEvent::{Presence, VoiceState, VoiceJoined, VoiceLeft}`, `Session::{join_voice, leave_voice, mute, deafen, start_share, stop_share}`.
- Produces: a members list grouped **In Voice / Online / Offline**; a channels rail with a **Voice** entry (join/leave) listing voice members; a self-panel (mute/deafen/share toggles).

- [ ] **Step 1: `ui/members.rs`** — `FactoryVecDeque<MemberRow>`; the parent feeds it the merged roster (online from `Presence`, voice subset from voice events), each row tagged with its group; render section headers + ● username (+ 🔊 if in voice).

- [ ] **Step 2: `ui/channels.rs`** — a static `# general` text row and a `🔊 Voice` row with a join/leave toggle (`join_voice`/`leave_voice`) and the voice-member names beneath it (from voice events).

- [ ] **Step 3: `ui/self_panel.rs`** — the logged-in username + `Mute`/`Deafen` `ToggleButton`s (→ `mute`/`deafen`) and a `Share` `ToggleButton` (→ `start_share`/`stop_share`).

- [ ] **Step 4: Wire** the three into `workspace.rs`; the root `app.rs` forwards the relevant `SessionEvent`s and ops.

- [ ] **Step 5: Build + run-and-observe** — two instances (alice, bob), distinct `HEARTH_TITLE`: both appear under Online; each clicks **Voice** → both show under **In Voice** and audio connects (engine logs `incoming voice linked`). Mute/Deafen toggle. Record in `desktop/README.md`.

- [ ] **Step 6: Commit**

```bash
git add desktop Cargo.lock && git commit -m "feat(desktop): members, channels, self-panel; join group voice"
```

---

## Task 7: Desktop — stage + chat + multi-sharer switcher

**Files:** Create `desktop/src/ui/{stage,chat}.rs`; Modify `desktop/src/ui/workspace.rs`, `desktop/src/app.rs`.

**Interfaces:**
- Consumes: `SessionEvent::{ShareStarted, ShareStopped, VideoReady, Chat, ChatHistory}`, `Session::{send_chat, paintable_for}`.
- Produces: a stage (`gtk::Picture` + a **Watching** switcher over the active sharer set) and a chat panel (`FactoryVecDeque` messages + entry) beneath it.

- [ ] **Step 1: `ui/chat.rs`** — `FactoryVecDeque<MessageRow>` fed from `ChatHistory`/`Chat`; a `gtk::Entry` whose activate emits `SendChat(body)` (→ `Session::send_chat`).

- [ ] **Step 2: `ui/stage.rs`** — a `gtk::Picture`; a row of toggle/radio buttons, one per active sharer (the parent maintains the sharer set from `ShareStarted`/`ShareStopped`). Selecting a sharer asks the parent for `paintable_for(peer, Screen)` and calls `picture.set_paintable(Some(&paintable))`. When the set is empty, the picture is cleared and chat fills the center.

- [ ] **Step 3: Wire** stage + chat into `workspace.rs` (stage on top, chat below); the root maintains `sharers: Vec<Uuid>` and the selected sharer, updating on events; new `VideoReady` for the currently-selected sharer re-mounts the paintable.

- [ ] **Step 4: Build + full run-and-observe** — 2–3 instances: join Voice (mutual audio), **alice and bob both Share**, a third (or one of them) sees **two entries in the Watching switcher** and **switches the stage between alice and bob instantly**; stop one share → switcher updates, the other keeps playing; chat works throughout. Record results in `desktop/README.md`.

- [ ] **Step 5: Commit**

```bash
git add desktop Cargo.lock && git commit -m "feat(desktop): stage with multi-sharer switcher + chat panel"
```

---

## Self-Review (completed during authoring)

- **Spec coverage:** protocol voice/share messages (T1), backend voice sub-room + share relay (T2), engine offerer rule + group voice mesh (T3), multi-viewer screenshare + `ScreenTransport` seam (T4), dark 3-pane shell + component split (T5), members/channels/self-panel + join voice (T6), stage switcher + chat (T7). North-star topology (voice mesh, P2P screenshare now, SFU-ready) and the glare rule are realized in T3/T4. Out-of-scope items (SFU, multiple voice channels, webcam, mobile) are intentionally absent.
- **Placeholder scan:** TDD code is concrete for protocol/backend/engine (T1–T4); UI tasks (T5–T7) are run-and-observe with explicit success criteria, matching the spec's testing section and the M5 precedent. No "TBD".
- **Type consistency:** the `ServerMessage`/`SessionEvent` voice+share variants, `should_offer(me, peer)`, `join_voice`/`leave_voice`/`start_share`/`stop_share`, `ScreenTransport::{start,stop}`, and `paintable_for(peer, Screen)` match across tasks and the engine API from M5.
- **Risk note:** acquiring `self_id` for the offerer rule (T3 Step 3) — read it from the JWT claims at `start`; if claims lack it, the fallback is to learn it from a self-named presence/voice event. The relm4 component split (T5–T7) is the bulk of the work; `FactoryVecDeque` handles the dynamic lists.
