# Hearth M3 — WebSocket Signaling Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Extend the existing authenticated WebSocket so peers can establish WebRTC calls through the Hearth server — relaying `join` / `offer` / `answer` / `ice` / `leave` between room members — replacing the throwaway `/tmp`-file signaling used by the M2 spike.

**Architecture:** A new `signaling` module adds a `SignalingHub` that holds one outbound channel per connected peer plus room membership, and routes messages either to a specific target peer (offer/answer/ice) or to the other members of a room (peer joined/left). The presence broadcast from M1 stays untouched; the WebSocket's send loop now multiplexes presence events and targeted signaling messages with `tokio::select!`. Everything is Rust and covered by hub unit tests plus a two-client WebSocket integration test.

**Tech Stack:** Rust, Axum 0.7 (WebSocket), Tokio (mpsc + broadcast + select), serde/serde_json, uuid 1, existing JWT auth.

## Global Constraints

- **Backend language:** Rust only. (Spec §2, §3.)
- **Signaling is a dumb relay:** the server never parses SDP/ICE media; it routes opaque blobs between the right peers. (Spec §3 "Signaling".)
- **Messages are explicit DTO structs/enums, no ad-hoc JSON.** (Spec §3.)
- **Auth:** the WebSocket is already authenticated via `?token=<access_jwt>`; the peer's identity (`sub`) comes from the verified JWT, never from the client message. (Spec §3.)
- **Rooms:** named channels; a 3-friend group typically uses one persistent room. (Spec §3.)
- **Commit cadence:** one commit per completed task; commit locally, do not push.
- **Run tests with:** `. "$HOME/.cargo/env"` sourced; Postgres dev container up on host port 5433 (`docker compose -f compose.dev.yml up -d postgres`). Unit tests: `cargo test --lib`. Integration: `cargo test --test <name>`.

---

## File Structure

```
backend/src/
├── signaling/
│   ├── mod.rs            # pub mod message; pub mod hub;
│   ├── message.rs        # ClientMessage + ServerMessage enums (the wire protocol)
│   └── hub.rs            # SignalingHub: per-peer senders + rooms + routing
├── state.rs              # AppState gains `signaling: SignalingHub`
└── presence/ws.rs        # send loop multiplexes presence + signaling; inbound parses ClientMessage
backend/tests/
└── signaling.rs          # two-client WS integration test
```

**Responsibilities & boundaries:**
- `message.rs` is pure data — serde enums, no logic, unit-testable by round-trip.
- `hub.rs` owns all routing/room state behind `Arc<Mutex<..>>` and the per-peer `mpsc` senders; it never touches HTTP/WebSocket types, so it is unit-testable with plain channels.
- `presence/ws.rs` is the only WebSocket-aware part: it registers/unregisters peers in the hub, parses inbound frames into `ClientMessage`, and forwards both presence and signaling messages out.

---

## Task 1: Signaling wire protocol (message types)

**Files:**
- Create: `backend/src/signaling/mod.rs`, `backend/src/signaling/message.rs`
- Modify: `backend/src/lib.rs`

**Interfaces:**
- Produces:
  - `signaling::message::ClientMessage` (serde-tagged, `#[serde(tag = "type", rename_all = "snake_case")]`):
    - `Join { room: String }`
    - `Offer { to: Uuid, sdp: String }`
    - `Answer { to: Uuid, sdp: String }`
    - `Ice { to: Uuid, mline: u32, candidate: String }`
    - `Leave`
  - `signaling::message::ServerMessage` (same tagging):
    - `RoomPeers { peers: Vec<PeerInfo> }`
    - `PeerJoined { user: Uuid, username: String }`
    - `PeerLeft { user: Uuid }`
    - `Offer { from: Uuid, sdp: String }`
    - `Answer { from: Uuid, sdp: String }`
    - `Ice { from: Uuid, mline: u32, candidate: String }`
  - `signaling::message::PeerInfo { user: Uuid, username: String }`

- [ ] **Step 1: Create `backend/src/signaling/message.rs` with the failing test only**

```rust
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_messages_parse_from_tagged_json() {
        let join: ClientMessage = serde_json::from_str(r#"{"type":"join","room":"main"}"#).unwrap();
        assert!(matches!(join, ClientMessage::Join { room } if room == "main"));

        let id = Uuid::now_v7();
        let raw = format!(r#"{{"type":"offer","to":"{id}","sdp":"v=0"}}"#);
        let offer: ClientMessage = serde_json::from_str(&raw).unwrap();
        assert!(matches!(offer, ClientMessage::Offer { to, sdp } if to == id && sdp == "v=0"));

        let leave: ClientMessage = serde_json::from_str(r#"{"type":"leave"}"#).unwrap();
        assert!(matches!(leave, ClientMessage::Leave));
    }

    #[test]
    fn server_messages_serialize_with_type_tag() {
        let id = Uuid::now_v7();
        let msg = ServerMessage::PeerJoined { user: id, username: "alice".into() };

        let v: serde_json::Value = serde_json::to_value(&msg).unwrap();
        assert_eq!(v["type"], "peer_joined");
        assert_eq!(v["user"], id.to_string());
        assert_eq!(v["username"], "alice");
    }
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cd backend && cargo test --lib signaling::message`
Expected: FAIL — `ClientMessage` / `ServerMessage` not defined / no `signaling` module.

- [ ] **Step 3: Implement the enums above the test module in `message.rs`**

```rust
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PeerInfo {
    pub user: Uuid,
    pub username: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMessage {
    Join { room: String },
    Offer { to: Uuid, sdp: String },
    Answer { to: Uuid, sdp: String },
    Ice { to: Uuid, mline: u32, candidate: String },
    Leave,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMessage {
    RoomPeers { peers: Vec<PeerInfo> },
    PeerJoined { user: Uuid, username: String },
    PeerLeft { user: Uuid },
    Offer { from: Uuid, sdp: String },
    Answer { from: Uuid, sdp: String },
    Ice { from: Uuid, mline: u32, candidate: String },
}
```

- [ ] **Step 4: Create `backend/src/signaling/mod.rs`**

```rust
pub mod hub;
pub mod message;
```

> `hub` is declared now so the module path is stable; its file is created in Task 2. If running tasks strictly one at a time and the crate must compile after Task 1, temporarily comment the `pub mod hub;` line and restore it in Task 2. (When running the whole plan in order, create `hub.rs` in Task 2 before building.)

- [ ] **Step 5: Register the module in `backend/src/lib.rs`**

Add `pub mod signaling;` to the module list (keep the list alphabetically ordered: it goes after `presence;`).

- [ ] **Step 6: Run the test to verify it passes**

Run: `cd backend && cargo test --lib signaling::message`
Expected: PASS (`client_messages_parse_from_tagged_json`, `server_messages_serialize_with_type_tag`).

- [ ] **Step 7: Commit**

```bash
git add backend/src/signaling backend/src/lib.rs
git commit -m "feat(backend): add signaling wire protocol message types"
```

---

## Task 2: SignalingHub (rooms + per-peer routing)

**Files:**
- Create: `backend/src/signaling/hub.rs`
- Modify: `backend/src/signaling/mod.rs` (ensure `pub mod hub;` is active)

**Interfaces:**
- Consumes: `signaling::message::{ServerMessage, PeerInfo}`.
- Produces:
  - `signaling::hub::SignalingHub` — `Clone` (Arc inside), `Default`.
  - `fn register(&self, user: Uuid, username: &str) -> tokio::sync::mpsc::UnboundedReceiver<ServerMessage>` — adds the peer and returns its outbound receiver.
  - `fn join_room(&self, user: Uuid, room: &str)` — sends `PeerJoined{user}` to existing room members, then sends `RoomPeers{existing}` to `user`, then records membership.
  - `fn relay(&self, to: Uuid, msg: ServerMessage)` — sends `msg` to one peer's channel (no-op if absent).
  - `fn leave_room(&self, user: Uuid)` — removes `user` from its room and sends `PeerLeft{user}` to remaining members (no-op if not in a room).
  - `fn disconnect(&self, user: Uuid)` — `leave_room` then drops the peer entirely.

- [ ] **Step 1: Write the failing test** at the bottom of `backend/src/signaling/hub.rs`

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::signaling::message::ServerMessage;
    use uuid::Uuid;

    #[tokio::test]
    async fn join_notifies_existing_members_and_returns_roster() {
        let hub = SignalingHub::default();
        let a = Uuid::now_v7();
        let b = Uuid::now_v7();

        let mut rx_a = hub.register(a, "alice");
        let mut rx_b = hub.register(b, "bob");

        hub.join_room(a, "main"); // alice alone: roster empty, nobody to notify
        let roster_a = rx_a.try_recv().unwrap();
        assert!(matches!(roster_a, ServerMessage::RoomPeers { peers } if peers.is_empty()));

        hub.join_room(b, "main"); // bob joins: alice hears peer_joined, bob gets roster [alice]
        let joined = rx_a.try_recv().unwrap();
        assert!(matches!(joined, ServerMessage::PeerJoined { user, .. } if user == b));

        let roster_b = rx_b.try_recv().unwrap();
        assert!(matches!(roster_b, ServerMessage::RoomPeers { peers } if peers.len() == 1 && peers[0].user == a));
    }

    #[tokio::test]
    async fn relay_targets_one_peer_and_disconnect_notifies_room() {
        let hub = SignalingHub::default();
        let a = Uuid::now_v7();
        let b = Uuid::now_v7();
        let mut rx_a = hub.register(a, "alice");
        let mut rx_b = hub.register(b, "bob");
        hub.join_room(a, "main");
        hub.join_room(b, "main");
        let _ = rx_a.try_recv(); // drain peer_joined(b)

        hub.relay(b, ServerMessage::Offer { from: a, sdp: "v=0".into() });
        let got = rx_b.try_recv().unwrap();
        assert!(matches!(got, ServerMessage::Offer { from, .. } if from == a));

        hub.disconnect(a);
        let left = rx_b.try_recv().unwrap();
        assert!(matches!(left, ServerMessage::PeerLeft { user } if user == a));
    }
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cd backend && cargo test --lib signaling::hub`
Expected: FAIL — `SignalingHub` not defined.

- [ ] **Step 3: Implement the hub above the test module**

```rust
use crate::signaling::message::{PeerInfo, ServerMessage};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;
use uuid::Uuid;

struct Peer {
    username: String,
    room: Option<String>,
    tx: mpsc::UnboundedSender<ServerMessage>,
}

#[derive(Clone, Default)]
pub struct SignalingHub {
    peers: Arc<Mutex<HashMap<Uuid, Peer>>>,
    rooms: Arc<Mutex<HashMap<String, HashSet<Uuid>>>>,
}

impl SignalingHub {
    pub fn register(&self, user: Uuid, username: &str) -> mpsc::UnboundedReceiver<ServerMessage> {
        let (tx, rx) = mpsc::unbounded_channel();

        self.peers.lock().unwrap().insert(user, Peer { username: username.to_string(), room: None, tx });

        rx
    }

    pub fn relay(&self, to: Uuid, msg: ServerMessage) {
        let peers = self.peers.lock().unwrap();

        if let Some(peer) = peers.get(&to) {
            let _ = peer.tx.send(msg);
        }
    }

    pub fn join_room(&self, user: Uuid, room: &str) {
        let mut peers = self.peers.lock().unwrap();
        let mut rooms = self.rooms.lock().unwrap();

        let members = rooms.entry(room.to_string()).or_default();

        let username = peers.get(&user).map(|p| p.username.clone()).unwrap_or_default();

        let existing: Vec<PeerInfo> = members
            .iter()
            .filter_map(|id| peers.get(id).map(|p| PeerInfo { user: *id, username: p.username.clone() }))
            .collect();

        for info in &existing {
            if let Some(p) = peers.get(&info.user) {
                let _ = p.tx.send(ServerMessage::PeerJoined { user, username: username.clone() });
            }
        }

        if let Some(p) = peers.get(&user) {
            let _ = p.tx.send(ServerMessage::RoomPeers { peers: existing });
        }

        members.insert(user);
        if let Some(p) = peers.get_mut(&user) {
            p.room = Some(room.to_string());
        }
    }

    pub fn leave_room(&self, user: Uuid) {
        let mut peers = self.peers.lock().unwrap();
        let mut rooms = self.rooms.lock().unwrap();

        let room = match peers.get_mut(&user).and_then(|p| p.room.take()) {
            Some(r) => r,
            None => return,
        };

        if let Some(members) = rooms.get_mut(&room) {
            members.remove(&user);

            for id in members.iter() {
                if let Some(p) = peers.get(id) {
                    let _ = p.tx.send(ServerMessage::PeerLeft { user });
                }
            }
        }
    }

    pub fn disconnect(&self, user: Uuid) {
        self.leave_room(user);

        self.peers.lock().unwrap().remove(&user);
    }
}
```

> `leave_room` takes both locks in the order `peers` then `rooms`; `join_room` uses the same order. Keeping a single lock order everywhere avoids deadlock.

- [ ] **Step 4: Ensure `pub mod hub;` is active** in `backend/src/signaling/mod.rs` (uncomment if it was commented in Task 1).

- [ ] **Step 5: Run the test to verify it passes**

Run: `cd backend && cargo test --lib signaling::hub`
Expected: PASS (both hub tests).

- [ ] **Step 6: Commit**

```bash
git add backend/src/signaling
git commit -m "feat(backend): add signaling hub with rooms and peer routing"
```

---

## Task 3: Wire signaling into the WebSocket + integration test

**Files:**
- Modify: `backend/src/state.rs`, `backend/src/presence/ws.rs`, `backend/src/main.rs`, `backend/tests/common/mod.rs`
- Create: `backend/tests/signaling.rs`

**Interfaces:**
- Consumes: `signaling::hub::SignalingHub`, `signaling::message::{ClientMessage, ServerMessage}`, existing `AuthUser`/JWT (`jwt::decode_access`), existing presence registry.
- Produces:
  - `AppState` gains `pub signaling: SignalingHub`.
  - `GET /ws?token=…` now: registers the peer in the hub; the send loop forwards both presence events and `ServerMessage`s; inbound text frames are parsed as `ClientMessage` and dispatched (`Join` → `join_room`; `Offer`/`Answer`/`Ice` → `relay` to `to` with `from` stamped to the sender; `Leave` → `leave_room`); on socket close → `presence.mark_offline` + `signaling.disconnect`.

- [ ] **Step 1: Write the failing integration test** `backend/tests/signaling.rs`

```rust
mod common;

use futures::{SinkExt, StreamExt};
use hearth_backend::{security::password, users::{entity::Role, repository}};
use tokio_tungstenite::tungstenite::Message;

async fn token(addr: &std::net::SocketAddr, name: &str) -> String {
    let client = reqwest::Client::new();
    let v: serde_json::Value = client.post(format!("http://{addr}/auth/login"))
        .json(&serde_json::json!({ "username": name, "password": "pw" }))
        .send().await.unwrap().json().await.unwrap();
    v["access_token"].as_str().unwrap().to_string()
}

/// Read frames until one with the given "type" arrives (ignores presence noise), or time out.
async fn wait_for_type(ws: &mut (impl StreamExt<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin), ty: &str) -> serde_json::Value {
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        while let Some(Ok(Message::Text(t))) = ws.next().await {
            let v: serde_json::Value = serde_json::from_str(&t).unwrap();
            if v["type"] == ty {
                return v;
            }
        }
        panic!("stream ended before a {ty} message");
    }).await.unwrap_or_else(|_| panic!("timed out waiting for {ty}"))
}

#[tokio::test]
async fn offer_and_ice_relay_between_two_peers_in_a_room() {
    let pool = common::test_pool().await;
    let addr = common::spawn_app().await;

    let a = format!("a_{}", uuid::Uuid::now_v7());
    let b = format!("b_{}", uuid::Uuid::now_v7());
    repository::create(&pool, &a, &password::hash("pw").unwrap(), Role::User).await.unwrap();
    repository::create(&pool, &b, &password::hash("pw").unwrap(), Role::User).await.unwrap();

    let ta = token(&addr, &a).await;
    let tb = token(&addr, &b).await;
    let (mut wsa, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/ws?token={ta}")).await.unwrap();
    let (mut wsb, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/ws?token={tb}")).await.unwrap();

    // Both join the same room. A joins first (alone), then B.
    wsa.send(Message::Text(r#"{"type":"join","room":"main"}"#.into())).await.unwrap();
    let roster_a = wait_for_type(&mut wsa, "room_peers").await;
    assert_eq!(roster_a["peers"].as_array().unwrap().len(), 0);

    wsb.send(Message::Text(r#"{"type":"join","room":"main"}"#.into())).await.unwrap();
    let roster_b = wait_for_type(&mut wsb, "room_peers").await;
    assert_eq!(roster_b["peers"].as_array().unwrap().len(), 1);

    // A learns B joined; capture B's id from that event.
    let joined = wait_for_type(&mut wsa, "peer_joined").await;
    let b_id = joined["user"].as_str().unwrap().to_string();

    // A sends an offer addressed to B; B receives it with from = A.
    wsa.send(Message::Text(format!(r#"{{"type":"offer","to":"{b_id}","sdp":"v=0"}}"#).into())).await.unwrap();
    let offer = wait_for_type(&mut wsb, "offer").await;
    assert_eq!(offer["sdp"], "v=0");
    let a_id = offer["from"].as_str().unwrap().to_string();

    // B answers A; then B sends an ICE candidate to A.
    wsb.send(Message::Text(format!(r#"{{"type":"answer","to":"{a_id}","sdp":"v=1"}}"#).into())).await.unwrap();
    let answer = wait_for_type(&mut wsa, "answer").await;
    assert_eq!(answer["sdp"], "v=1");

    wsb.send(Message::Text(format!(r#"{{"type":"ice","to":"{a_id}","mline":0,"candidate":"cand"}}"#).into())).await.unwrap();
    let ice = wait_for_type(&mut wsa, "ice").await;
    assert_eq!(ice["candidate"], "cand");

    // B disconnects; A is told B left.
    drop(wsb);
    let left = wait_for_type(&mut wsa, "peer_left").await;
    assert_eq!(left["user"], b_id);
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cd backend && cargo test --test signaling`
Expected: FAIL — `AppState` has no `signaling` field / inbound frames are ignored, so no `room_peers` ever arrives (times out).

- [ ] **Step 3: Add `signaling` to `AppState`** in `backend/src/state.rs`

```rust
use crate::config::AppConfig;
use crate::presence::registry::PresenceRegistry;
use crate::signaling::hub::SignalingHub;
use sqlx::PgPool;

#[derive(Clone)]
pub struct AppState {
    pub pool: PgPool,
    pub config: AppConfig,
    pub presence: PresenceRegistry,
    pub signaling: SignalingHub,
}
```

- [ ] **Step 4: Construct it in `main.rs`** — change the state init line

```rust
let state = AppState { pool, config, presence: PresenceRegistry::new(), signaling: SignalingHub::default() };
```

(Add `signaling::hub::SignalingHub` to the `use hearth_backend::{…}` import list.)

- [ ] **Step 5: Construct it in the test harness** `backend/tests/common/mod.rs` — change the state init line

```rust
let state = AppState {
    pool,
    config: test_config(),
    presence: PresenceRegistry::new(),
    signaling: hearth_backend::signaling::hub::SignalingHub::default(),
};
```

- [ ] **Step 6: Rewrite `handle_socket` in `backend/src/presence/ws.rs`** to multiplex presence + signaling and dispatch inbound frames

Replace the existing `handle_socket` function body with:

```rust
async fn handle_socket(socket: WebSocket, state: AppState, id: uuid::Uuid, username: String) {
    let mut presence_rx = state.presence.subscribe();
    let mut sig_rx = state.signaling.register(id, &username);
    state.presence.mark_online(id, &username);

    let (mut sink, mut stream) = socket.split();

    // Outbound: forward presence events and targeted signaling messages.
    let forward = tokio::spawn(async move {
        loop {
            tokio::select! {
                presence = presence_rx.recv() => match presence {
                    Ok(event) => {
                        let json = serde_json::to_string(&event).unwrap();
                        if sink.send(Message::Text(json)).await.is_err() { break; }
                    }
                    Err(_) => {} // lagged/closed broadcast: keep serving signaling
                },
                signal = sig_rx.recv() => match signal {
                    Some(msg) => {
                        let json = serde_json::to_string(&msg).unwrap();
                        if sink.send(Message::Text(json)).await.is_err() { break; }
                    }
                    None => break, // hub dropped this peer's sender
                },
            }
        }
    });

    // Inbound: parse client signaling messages and route them through the hub.
    while let Some(Ok(msg)) = stream.next().await {
        match msg {
            Message::Text(text) => {
                if let Ok(cm) = serde_json::from_str::<crate::signaling::message::ClientMessage>(&text) {
                    dispatch(&state, id, cm);
                }
            }
            Message::Close(_) => break,
            _ => {}
        }
    }

    forward.abort();
    state.signaling.disconnect(id);
    state.presence.mark_offline(id, &username);
}

fn dispatch(state: &AppState, from: uuid::Uuid, msg: crate::signaling::message::ClientMessage) {
    use crate::signaling::message::{ClientMessage, ServerMessage};

    match msg {
        ClientMessage::Join { room } => state.signaling.join_room(from, &room),
        ClientMessage::Offer { to, sdp } => state.signaling.relay(to, ServerMessage::Offer { from, sdp }),
        ClientMessage::Answer { to, sdp } => state.signaling.relay(to, ServerMessage::Answer { from, sdp }),
        ClientMessage::Ice { to, mline, candidate } => state.signaling.relay(to, ServerMessage::Ice { from, mline, candidate }),
        ClientMessage::Leave => state.signaling.leave_room(from),
    }
}
```

> The `Message::Text(json)` send keeps the M1 form (axum 0.7 `Text(String)`). The send loop now uses `tokio::select!`; presence broadcast `Err` (lag) is non-fatal so signaling keeps working.

- [ ] **Step 7: Run the signaling integration test**

Run: `cd backend && cargo test --test signaling`
Expected: PASS (`offer_and_ice_relay_between_two_peers_in_a_room`).

- [ ] **Step 8: Run the full suite to confirm nothing regressed (presence especially)**

Run: `cd backend && cargo test`
Expected: PASS — health, db, users, auth, users_admin, presence, signaling, plus lib unit tests (security, signaling::message, signaling::hub).

- [ ] **Step 9: Commit**

```bash
git add backend/src/state.rs backend/src/presence/ws.rs backend/src/main.rs backend/tests/common/mod.rs backend/tests/signaling.rs
git commit -m "feat(backend): relay WebRTC signaling over the WebSocket"
```

---

## Self-Review (completed during authoring)

- **Spec coverage:** §3 "Signaling" — the WebSocket now carries `join_room`/`offer`/`answer`/`ice_candidate`/`leave` and peer join/leave events, as a pure relay that never parses media (Tasks 1-3). Identity is taken from the verified JWT, not the client message (Task 3 `dispatch` stamps `from = id`). Rooms are named channels (Task 2). Presence (M1) is preserved and multiplexed, not replaced.
- **Placeholder scan:** no TBD/TODO; every step has concrete code and exact run commands. The only conditional note (Task 1 Step 4 `pub mod hub;`) is an explicit ordering instruction with both branches spelled out, not a placeholder.
- **Type consistency:** `ClientMessage`/`ServerMessage`/`PeerInfo` field names (`to`, `from`, `sdp`, `mline`, `candidate`, `room`, `user`, `username`, `peers`) are identical across message.rs, hub.rs, the hub tests, ws.rs `dispatch`, and the integration test. `SignalingHub::{register, join_room, relay, leave_room, disconnect}` signatures match between hub.rs and the call sites in ws.rs. `AppState` field `signaling` is added in state.rs and constructed in both main.rs and the test harness.
- **Boundary check:** hub.rs has zero HTTP/WS imports (channels only) → unit-testable without a server; ws.rs is the only file touching both the hub and axum.
