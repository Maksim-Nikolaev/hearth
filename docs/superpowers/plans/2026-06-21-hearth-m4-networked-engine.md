# Hearth M4 — Networked Media Engine Peer Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Turn the throwaway M2 spike into a real, cross-platform `engine` crate that performs hardware-encoded P2P screenshare driven by the Hearth WebSocket signaling server (the M3 protocol), so two peers — on Linux and Windows — can connect through the server with no `/tmp` files. This unlocks the cross-machine latency/NAT measurement.

**Architecture:** A shared `hearth-protocol` crate holds the signaling message enums (consumed by both backend and engine). The `engine` crate has a `signaling` client (REST login → authenticated WebSocket, typed send/recv), per-OS `capture` selection, runtime `encoders` detection (carried over from the spike), and a `peer` that wires a GStreamer `webrtcbin` pipeline to the signaling client: local ICE/offer/answer flow out as `ClientMessage`s, remote ones arrive as `ServerMessage`s and drive the pipeline. A thin CLI binary runs one peer in `share` (offerer/capture) or `view` (answerer/display) mode.

**Tech Stack:** Rust, GStreamer 1.24 via gstreamer-rs 0.23 (`webrtcbin`, VA-API/AMF encoders), Tokio, tokio-tungstenite (WS client), reqwest (rustls) for login, serde, the existing Hearth backend (M3) as the signaling server.

## Global Constraints

- **Shared protocol:** signaling message types live in one crate (`hearth-protocol`); backend and engine both depend on it. No duplicated message definitions. (Spec §2 boundaries.)
- **Signaling is a dumb relay:** the engine sends/receives opaque SDP/ICE through the server; the server never parses media. (Spec §3.)
- **Identity from auth:** the engine logs in (username+password → JWT) and connects `?token=<jwt>`; peer ids come from the server, never trusted from a message body. (Spec §3.)
- **Cross-platform from the start:** capture and encoder selection are abstracted per-OS — Linux/X11 (`ximagesrc` + VA-API) is the verified path; Windows (`d3d11screencapturesrc` + AMF) is built and must be confirmed on a Windows box. (Spec §1 MVP = Windows + Linux; Spec §4 OBS-style encoder detection.)
- **Engine is product code** (unlike `engine-spike`): it becomes the S2 media engine that `flutter_rust_bridge` will later wrap. Keep modules small and FFI-friendly (no `main`-only logic).
- **Commit cadence:** one commit per completed task; commit locally, do not push.
- **Run env:** `. "$HOME/.cargo/env"` sourced; Postgres dev container up on host port 5433. GStreamer dev packages + libnice already installed (M2). Unit tests: `cargo test` in the crate. The networked loopback (Task 4) needs the backend running.

---

## File Structure

```
hearth/
├── protocol/                      # NEW shared crate: hearth-protocol
│   ├── Cargo.toml
│   └── src/lib.rs                 # PeerInfo, ClientMessage, ServerMessage (Serialize + Deserialize) + tests
├── backend/
│   ├── Cargo.toml                 # + hearth-protocol path dep
│   └── src/signaling/message.rs   # becomes: pub use hearth_protocol::*;
└── engine/                        # NEW product crate (replaces engine-spike's role)
    ├── Cargo.toml
    └── src/
        ├── lib.rs                 # pub mod {encoders, capture, signaling, peer};
        ├── encoders.rs            # runtime HW encoder probe (from spike)
        ├── capture.rs             # per-OS capture chain string
        ├── signaling.rs           # login + WS client: typed send/recv
        ├── peer.rs                # webrtcbin pipeline driven by signaling
        └── main.rs                # CLI: share | view
```

`engine-spike/` stays as-is (historical reference); it is not modified or deleted by this plan.

**Boundaries:**
- `hearth-protocol` is pure data; both backend and engine depend on it, nothing depends back.
- `signaling.rs` knows WebSockets + protocol, nothing about GStreamer.
- `capture.rs`/`encoders.rs` are pure pipeline-string/element selection, unit-testable.
- `peer.rs` is the only place that joins GStreamer to the signaling client.

---

## Task 1: Extract the shared `hearth-protocol` crate

**Files:**
- Create: `protocol/Cargo.toml`, `protocol/src/lib.rs`
- Modify: `backend/Cargo.toml`, `backend/src/signaling/message.rs`

**Interfaces:**
- Produces: `hearth_protocol::{PeerInfo, ClientMessage, ServerMessage}` — same shapes as M3, but **both** enums now derive `Serialize + Deserialize` (the client serializes `ClientMessage`/deserializes `ServerMessage`; the server does the reverse).
- `backend::signaling::message` re-exports these, so all existing `crate::signaling::message::*` paths keep working unchanged.

- [ ] **Step 1: Create `protocol/Cargo.toml`**

```toml
[package]
name = "hearth-protocol"
version = "0.1.0"
edition = "2021"

[dependencies]
serde = { version = "1", features = ["derive"] }
uuid = { version = "1", features = ["v7", "serde"] }

[dev-dependencies]
serde_json = "1"
```

- [ ] **Step 2: Create `protocol/src/lib.rs`** with the enums (both directions) + the round-trip tests moved from the backend

```rust
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PeerInfo {
    pub user: Uuid,
    pub username: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMessage {
    Join { room: String },
    Offer { to: Uuid, sdp: String },
    Answer { to: Uuid, sdp: String },
    Ice { to: Uuid, mline: u32, candidate: String },
    Leave,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMessage {
    RoomPeers { peers: Vec<PeerInfo> },
    PeerJoined { user: Uuid, username: String },
    PeerLeft { user: Uuid },
    Offer { from: Uuid, sdp: String },
    Answer { from: Uuid, sdp: String },
    Ice { from: Uuid, mline: u32, candidate: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_message_round_trips() {
        let id = Uuid::now_v7();
        let msg = ClientMessage::Offer { to: id, sdp: "v=0".into() };

        let json = serde_json::to_string(&msg).unwrap();
        let back: ClientMessage = serde_json::from_str(&json).unwrap();

        assert_eq!(msg, back);
        assert!(json.contains("\"type\":\"offer\""));
    }

    #[test]
    fn server_message_round_trips() {
        let id = Uuid::now_v7();
        let msg = ServerMessage::PeerJoined { user: id, username: "alice".into() };

        let json = serde_json::to_string(&msg).unwrap();
        let back: ServerMessage = serde_json::from_str(&json).unwrap();

        assert_eq!(msg, back);
        assert!(json.contains("\"type\":\"peer_joined\""));
    }
}
```

- [ ] **Step 3: Run the protocol tests to verify they pass**

Run: `cd protocol && cargo test`
Expected: PASS (`client_message_round_trips`, `server_message_round_trips`).

- [ ] **Step 4: Add the path dep to `backend/Cargo.toml`** (under `[dependencies]`, after `uuid`)

```toml
hearth-protocol = { path = "../protocol" }
```

- [ ] **Step 5: Replace `backend/src/signaling/message.rs` with a re-export**

```rust
pub use hearth_protocol::{ClientMessage, PeerInfo, ServerMessage};
```

- [ ] **Step 6: Run the full backend suite to confirm no regression**

Run: `cd backend && cargo test`
Expected: PASS — all M0-M3 tests (auth, db, health, presence, signaling, users, users_admin, lib unit). The `signaling::message::tests` unit tests no longer live in backend (they moved to protocol); the backend's `signaling.rs` integration test still validates the wire format end-to-end.

- [ ] **Step 7: Commit**

```bash
git add protocol backend/Cargo.toml backend/Cargo.lock backend/src/signaling/message.rs
git commit -m "refactor: extract shared hearth-protocol crate"
```

---

## Task 2: Engine crate scaffold — encoders + per-OS capture

**Files:**
- Create: `engine/Cargo.toml`, `engine/src/lib.rs`, `engine/src/encoders.rs`, `engine/src/capture.rs`, `engine/src/main.rs`

**Interfaces:**
- Produces:
  - `engine::encoders::detect() -> (Option<&'static str>, Vec<(&'static str, &'static str, bool)>)` — first available HW HEVC encoder + availability list (carried from the spike).
  - `engine::capture::capture_chain() -> &'static str` — a GStreamer sub-pipeline string ending at system-memory video, selected per OS.
  - A `main` that accepts a subcommand (`probe` for now) so the crate has a runnable binary.

- [ ] **Step 1: Create `engine/Cargo.toml`**

```toml
[package]
name = "engine"
version = "0.1.0"
edition = "2021"

[dependencies]
gstreamer = "0.23"
gstreamer-webrtc = "0.23"
gstreamer-sdp = "0.23"
tokio = { version = "1", features = ["full"] }
tokio-tungstenite = "0.24"
futures = "0.3"
reqwest = { version = "0.12", default-features = false, features = ["json", "rustls-tls"] }
serde_json = "1"
anyhow = "1"
uuid = { version = "1", features = ["v7", "serde"] }
hearth-protocol = { path = "../protocol" }
```

- [ ] **Step 2: Write `engine/src/encoders.rs`** with the probe + a deterministic unit test

```rust
use gstreamer as gst;

const CANDIDATES: &[(&str, &str)] = &[
    ("amfh265enc", "AMD AMF HEVC"),
    ("vah265enc", "VA-API HEVC (modern)"),
    ("vaapih265enc", "VA-API HEVC (legacy)"),
    ("nvh265enc", "NVIDIA NVENC HEVC"),
    ("qsvh265enc", "Intel QuickSync HEVC"),
    ("vtenc_h265", "Apple VideoToolbox HEVC"),
    ("x265enc", "software HEVC (fallback)"),
];

/// First available encoder factory name, plus the full availability list.
pub fn detect() -> (Option<&'static str>, Vec<(&'static str, &'static str, bool)>) {
    let mut list = Vec::new();
    let mut chosen = None;

    for (factory, label) in CANDIDATES {
        let available = gst::ElementFactory::find(factory).is_some();

        if available && chosen.is_none() {
            chosen = Some(*factory);
        }

        list.push((*factory, *label, available));
    }

    (chosen, list)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_is_consistent() {
        gst::init().unwrap();

        let (chosen, list) = detect();

        assert_eq!(list.len(), 7);
        assert_eq!(chosen.is_some(), list.iter().any(|(_, _, ok)| *ok));
    }
}
```

- [ ] **Step 3: Write `engine/src/capture.rs`** with the per-OS chain + a unit test

```rust
/// GStreamer sub-pipeline that captures the screen and outputs system-memory
/// video frames (ready for an encoder). Selected per OS.
pub fn capture_chain() -> &'static str {
    #[cfg(target_os = "linux")]
    {
        "ximagesrc use-damage=false ! videoconvert"
    }
    #[cfg(target_os = "windows")]
    {
        // d3d11screencapturesrc yields GPU memory; download to system memory for the encoder.
        "d3d11screencapturesrc ! d3d11download ! videoconvert"
    }
    #[cfg(not(any(target_os = "linux", target_os = "windows")))]
    {
        "videotestsrc ! videoconvert"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_chain_is_present_and_converts() {
        let chain = capture_chain();

        assert!(!chain.is_empty());
        assert!(chain.contains("videoconvert"));
    }
}
```

- [ ] **Step 4: Write `engine/src/lib.rs`**

```rust
pub mod capture;
pub mod encoders;
```

(`signaling` and `peer` modules are added in Tasks 3-4.)

- [ ] **Step 5: Write `engine/src/main.rs`** (probe subcommand for now)

```rust
fn main() -> anyhow::Result<()> {
    gstreamer::init()?;

    let mode = std::env::args().nth(1).unwrap_or_else(|| "probe".into());

    match mode.as_str() {
        "probe" => {
            let (chosen, list) = engine::encoders::detect();

            for (factory, label, ok) in &list {
                println!("[{}] {:<14} {}", if *ok { "x" } else { " " }, factory, label);
            }

            println!("capture chain: {}", engine::capture::capture_chain());
            println!("selected encoder: {chosen:?}");
        }
        other => anyhow::bail!("unknown mode: {other}"),
    }

    Ok(())
}
```

- [ ] **Step 6: Run the engine unit tests**

Run: `cd engine && cargo test`
Expected: PASS (`encoders::tests::detect_is_consistent`, `capture::tests::capture_chain_is_present_and_converts`).

- [ ] **Step 7: Run the probe to sanity-check on this box**

Run: `cd engine && cargo run -- probe`
Expected: `vah265enc` selected, capture chain shows the `ximagesrc` (Linux) variant.

- [ ] **Step 8: Commit**

```bash
git add engine
git commit -m "feat(engine): scaffold engine crate with encoder probe and per-OS capture"
```

---

## Task 3: Signaling client (login + typed WebSocket)

**Files:**
- Create: `engine/src/signaling.rs`
- Modify: `engine/src/lib.rs`

**Interfaces:**
- Produces:
  - `engine::signaling::login(http_base: &str, username: &str, password: &str) -> anyhow::Result<String>` — POSTs `/auth/login`, returns the access token.
  - `engine::signaling::SignalingClient` with:
    - `async fn connect(ws_base: &str, token: &str) -> anyhow::Result<(SignalingClient, tokio::sync::mpsc::UnboundedReceiver<ServerMessage>)>` — opens `ws_base/ws?token=…`, returns the client + a receiver of inbound `ServerMessage`s.
    - `fn send(&self, msg: ClientMessage)` — queues an outbound message (non-blocking).
- Consumes: `hearth_protocol::{ClientMessage, ServerMessage}`.

- [ ] **Step 1: Write the failing integration test** at the bottom of `engine/src/signaling.rs` (mock WS server in-test; no backend needed)

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use futures::{SinkExt, StreamExt};
    use hearth_protocol::{ClientMessage, ServerMessage};
    use tokio::net::TcpListener;
    use tokio_tungstenite::tungstenite::Message;

    // Minimal server: accept one ws conn, expect a `join`, reply with empty `room_peers`.
    async fn mock_server() -> std::net::SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();

            let first = ws.next().await.unwrap().unwrap();
            let text = first.into_text().unwrap();
            let cm: ClientMessage = serde_json::from_str(&text).unwrap();
            assert!(matches!(cm, ClientMessage::Join { .. }));

            let reply = ServerMessage::RoomPeers { peers: vec![] };
            ws.send(Message::Text(serde_json::to_string(&reply).unwrap())).await.unwrap();

            // keep the socket open briefly so the client can read
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        });

        addr
    }

    #[tokio::test]
    async fn sends_join_and_receives_room_peers() {
        let addr = mock_server().await;

        // `connect` appends "/ws?token=..."; the mock ignores the path, so any base works.
        let (client, mut inbound) = SignalingClient::connect(&format!("ws://{addr}"), "tok").await.unwrap();

        client.send(ClientMessage::Join { room: "main".into() });

        let msg = tokio::time::timeout(std::time::Duration::from_secs(2), inbound.recv())
            .await.unwrap().unwrap();

        assert!(matches!(msg, ServerMessage::RoomPeers { peers } if peers.is_empty()));
    }
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cd engine && cargo test signaling::tests`
Expected: FAIL — `SignalingClient` not defined.

- [ ] **Step 3: Implement the client above the test module**

```rust
use anyhow::{anyhow, Result};
use futures::{SinkExt, StreamExt};
use hearth_protocol::{ClientMessage, ServerMessage};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;

pub async fn login(http_base: &str, username: &str, password: &str) -> Result<String> {
    let client = reqwest::Client::new();

    let resp = client
        .post(format!("{http_base}/auth/login"))
        .json(&serde_json::json!({ "username": username, "password": password }))
        .send()
        .await?;

    if !resp.status().is_success() {
        return Err(anyhow!("login failed: {}", resp.status()));
    }

    let body: serde_json::Value = resp.json().await?;
    body["access_token"].as_str().map(str::to_string).ok_or_else(|| anyhow!("no access_token in response"))
}

pub struct SignalingClient {
    out_tx: mpsc::UnboundedSender<ClientMessage>,
}

impl SignalingClient {
    pub async fn connect(ws_base: &str, token: &str) -> Result<(Self, mpsc::UnboundedReceiver<ServerMessage>)> {
        let url = format!("{ws_base}/ws?token={token}");
        let (ws, _) = tokio_tungstenite::connect_async(url).await?;
        let (mut sink, mut stream) = ws.split();

        let (out_tx, mut out_rx) = mpsc::unbounded_channel::<ClientMessage>();
        let (in_tx, in_rx) = mpsc::unbounded_channel::<ServerMessage>();

        // Outbound: serialize queued ClientMessages onto the socket.
        tokio::spawn(async move {
            while let Some(msg) = out_rx.recv().await {
                let json = serde_json::to_string(&msg).unwrap();
                if sink.send(Message::Text(json)).await.is_err() {
                    break;
                }
            }
        });

        // Inbound: parse text frames into ServerMessages.
        tokio::spawn(async move {
            while let Some(Ok(frame)) = stream.next().await {
                if let Message::Text(text) = frame {
                    if let Ok(msg) = serde_json::from_str::<ServerMessage>(&text) {
                        if in_tx.send(msg).is_err() {
                            break;
                        }
                    }
                }
            }
        });

        Ok((Self { out_tx }, in_rx))
    }

    pub fn send(&self, msg: ClientMessage) {
        let _ = self.out_tx.send(msg);
    }
}
```

- [ ] **Step 4: Register the module** — add `pub mod signaling;` to `engine/src/lib.rs`

- [ ] **Step 5: Run the test to verify it passes**

Run: `cd engine && cargo test signaling::tests`
Expected: PASS (`sends_join_and_receives_room_peers`).

- [ ] **Step 6: Commit**

```bash
git add engine
git commit -m "feat(engine): add signaling client (login + typed WebSocket)"
```

---

## Task 4: webrtcbin peer driven by the signaling client (run & observe)

> Like the M2 spike's Task C, the end-to-end media path can't be unit-tested (needs the backend running, GStreamer, and a display). The code is complete; verification is a **loopback run with success criteria**.

**Files:**
- Create: `engine/src/peer.rs`
- Modify: `engine/src/lib.rs`, `engine/src/main.rs`

**Interfaces:**
- Consumes: `engine::signaling::{login, SignalingClient}`, `engine::{capture, encoders}`, `hearth_protocol::{ClientMessage, ServerMessage}`.
- Produces: `engine::peer::run(http_base, ws_base, username, password, room, share: bool) -> anyhow::Result<()>` — logs in, joins the room, and runs a `webrtcbin` session: `share=true` captures+sends this screen to the first other peer; `share=false` receives+displays.

- [ ] **Step 1: Implement `engine/src/peer.rs`**

```rust
use crate::{capture, encoders, signaling::{login, SignalingClient}};
use anyhow::Result;
use gstreamer as gst;
use gstreamer::glib;
use gstreamer::prelude::*;
use gstreamer_sdp as gst_sdp;
use gstreamer_webrtc as gst_webrtc;
use hearth_protocol::{ClientMessage, ServerMessage};
use std::sync::{Arc, Mutex};
use uuid::Uuid;

struct State {
    target: Mutex<Option<Uuid>>,
    pending_ice: Mutex<Vec<(u32, String)>>,
    offer_created: Mutex<bool>,
}

pub async fn run(http_base: &str, ws_base: &str, username: &str, password: &str, room: &str, share: bool) -> Result<()> {
    gst::init()?;

    let token = login(http_base, username, password).await?;
    let (client, mut inbound) = SignalingClient::connect(ws_base, &token).await?;
    let client = Arc::new(client);
    let state = Arc::new(State { target: Mutex::new(None), pending_ice: Mutex::new(Vec::new()), offer_created: Mutex::new(false) });

    let pipeline = gst::Pipeline::new();
    let webrtc = gst::ElementFactory::make("webrtcbin")
        .name("wrtc")
        .property_from_str("stun-server", "stun://stun.l.google.com:19302")
        .build()?;
    pipeline.add(&webrtc)?;

    if share {
        let encoder = encoders::detect().0.unwrap_or("x265enc");
        let desc = format!(
            "{} ! {encoder} ! h265parse ! rtph265pay config-interval=-1 ! application/x-rtp,media=video,encoding-name=H265,payload=96 ! wrtc.",
            capture::capture_chain()
        );
        // Build the send branch as a bin and add it linked to the existing webrtcbin.
        let bin = gst::parse::bin_from_description_with_name(&desc, false, "sendbin")?;
        pipeline.add(&bin)?;
        // parse::bin_from_description with the trailing `wrtc.` cannot see our webrtc; instead
        // link the bin's src ghost pad to webrtcbin. Simpler: build elements explicitly below.
        pipeline.remove(&bin)?;

        build_send_branch(&pipeline, &webrtc, encoder)?;
    }

    // Incoming media (viewer): decode + display.
    let pipeline_weak = pipeline.downgrade();
    webrtc.connect_pad_added(move |_w, pad| {
        if pad.direction() != gst::PadDirection::Src { return; }
        let Some(pipeline) = pipeline_weak.upgrade() else { return };

        let depay = gst::ElementFactory::make("rtph265depay").build().unwrap();
        let parse = gst::ElementFactory::make("h265parse").build().unwrap();
        let dec = gst::ElementFactory::make("avdec_h265").build().unwrap();
        let conv = gst::ElementFactory::make("videoconvert").build().unwrap();
        let sink = gst::ElementFactory::make("autovideosink").property("sync", false).build().unwrap();

        pipeline.add_many([&depay, &parse, &dec, &conv, &sink]).unwrap();
        gst::Element::link_many([&depay, &parse, &dec, &conv, &sink]).unwrap();
        for e in [&depay, &parse, &dec, &conv, &sink] { e.sync_state_with_parent().unwrap(); }
        pad.link(&depay.static_pad("sink").unwrap()).unwrap();
        println!("incoming stream linked -> displaying");
    });

    // Local ICE -> signaling (buffer until we know the target).
    {
        let client = client.clone();
        let state = state.clone();
        webrtc.connect("on-ice-candidate", false, move |vals| {
            let mline = vals[1].get::<u32>().unwrap();
            let cand = vals[2].get::<String>().unwrap();

            let target = *state.target.lock().unwrap();
            match target {
                Some(to) => client.send(ClientMessage::Ice { to, mline, candidate: cand }),
                None => state.pending_ice.lock().unwrap().push((mline, cand)),
            }
            None
        });
    }

    webrtc.connect_notify(Some("connection-state"), |w, _| {
        let s = w.property::<gst_webrtc::WebRTCPeerConnectionState>("connection-state");
        println!("connection-state: {s:?}");
    });

    pipeline.set_state(gst::State::Playing)?;
    client.send(ClientMessage::Join { room: room.to_string() });

    // Drive negotiation from inbound signaling on a blocking-friendly task.
    let webrtc_loop = webrtc.clone();
    let client_loop = client.clone();
    let state_loop = state.clone();
    let main_loop = glib::MainLoop::new(None, false);
    let ml = main_loop.clone();

    tokio::spawn(async move {
        while let Some(msg) = inbound.recv().await {
            handle_signal(&webrtc_loop, &client_loop, &state_loop, share, msg);
        }
        ml.quit();
    });

    main_loop.run();
    pipeline.set_state(gst::State::Null)?;
    Ok(())
}

fn build_send_branch(pipeline: &gst::Pipeline, webrtc: &gst::Element, encoder: &str) -> Result<()> {
    // Linux: ximagesrc; Windows: d3d11screencapturesrc + d3d11download. capture_chain() encodes the OS choice.
    let cap = gst::parse::bin_from_description(capture::capture_chain(), true)?;
    let enc = gst::ElementFactory::make(encoder).build()?;
    let parse = gst::ElementFactory::make("h265parse").build()?;
    let pay = gst::ElementFactory::make("rtph265pay").property("config-interval", -1i32).build()?;
    let caps = gst::ElementFactory::make("capsfilter")
        .property("caps", gst::Caps::builder("application/x-rtp")
            .field("media", "video").field("encoding-name", "H265").field("payload", 96i32).build())
        .build()?;

    pipeline.add_many([cap.upcast_ref(), &enc, &parse, &pay, &caps])?;
    gst::Element::link_many([cap.upcast_ref(), &enc, &parse, &pay, &caps])?;
    caps.link(webrtc)?;
    Ok(())
}

fn handle_signal(webrtc: &gst::Element, client: &SignalingClient, state: &Arc<State>, share: bool, msg: ServerMessage) {
    match msg {
        ServerMessage::RoomPeers { peers } => {
            if share {
                if let Some(p) = peers.first() {
                    set_target_and_offer(webrtc, client, state, p.user);
                }
            }
        }
        ServerMessage::PeerJoined { user, .. } => {
            if share {
                set_target_and_offer(webrtc, client, state, user);
            }
        }
        ServerMessage::Offer { from, sdp } => {
            *state.target.lock().unwrap() = Some(from);
            flush_ice(client, state);

            let sdp = gst_sdp::SDPMessage::parse_buffer(sdp.as_bytes()).unwrap();
            let offer = gst_webrtc::WebRTCSessionDescription::new(gst_webrtc::WebRTCSDPType::Offer, sdp);
            webrtc.emit_by_name::<()>("set-remote-description", &[&offer, &None::<gst::Promise>]);

            let w = webrtc.clone();
            let c_target = from;
            let client_ptr = client as *const SignalingClient;
            // Create the answer; send it back to `from`.
            let promise = gst::Promise::with_change_func(move |reply| {
                let Ok(Some(reply)) = reply else { return };
                let answer = reply.value("answer").unwrap().get::<gst_webrtc::WebRTCSessionDescription>().unwrap();
                w.emit_by_name::<()>("set-local-description", &[&answer, &None::<gst::Promise>]);
                // SAFETY: client outlives the main loop; this closure runs on the glib thread synchronously.
                let client = unsafe { &*client_ptr };
                client.send(ClientMessage::Answer { to: c_target, sdp: answer.sdp().as_text().unwrap().to_string() });
            });
            webrtc.emit_by_name::<()>("create-answer", &[&None::<gst::Structure>, &promise]);
        }
        ServerMessage::Answer { from: _, sdp } => {
            let sdp = gst_sdp::SDPMessage::parse_buffer(sdp.as_bytes()).unwrap();
            let answer = gst_webrtc::WebRTCSessionDescription::new(gst_webrtc::WebRTCSDPType::Answer, sdp);
            webrtc.emit_by_name::<()>("set-remote-description", &[&answer, &None::<gst::Promise>]);
        }
        ServerMessage::Ice { from: _, mline, candidate } => {
            webrtc.emit_by_name::<()>("add-ice-candidate", &[&mline, &candidate]);
        }
        ServerMessage::PeerLeft { .. } => {}
    }
}

fn set_target_and_offer(webrtc: &gst::Element, client: &SignalingClient, state: &Arc<State>, target: Uuid) {
    {
        let mut t = state.target.lock().unwrap();
        if t.is_some() { return; }
        *t = Some(target);
    }
    flush_ice(client, state);

    let mut created = state.offer_created.lock().unwrap();
    if *created { return; }
    *created = true;

    let w = webrtc.clone();
    let client_ptr = client as *const SignalingClient;
    let promise = gst::Promise::with_change_func(move |reply| {
        let Ok(Some(reply)) = reply else { return };
        let offer = reply.value("offer").unwrap().get::<gst_webrtc::WebRTCSessionDescription>().unwrap();
        w.emit_by_name::<()>("set-local-description", &[&offer, &None::<gst::Promise>]);
        let client = unsafe { &*client_ptr };
        client.send(ClientMessage::Offer { to: target, sdp: offer.sdp().as_text().unwrap().to_string() });
    });
    webrtc.emit_by_name::<()>("create-offer", &[&None::<gst::Structure>, &promise]);
}

fn flush_ice(client: &SignalingClient, state: &Arc<State>) {
    let target = match *state.target.lock().unwrap() { Some(t) => t, None => return };
    let mut pending = state.pending_ice.lock().unwrap();
    for (mline, candidate) in pending.drain(..) {
        client.send(ClientMessage::Ice { to: target, mline, candidate });
    }
}
```

> The raw-pointer `unsafe { &*client_ptr }` passes the `SignalingClient` (its cheap `UnboundedSender`) into the GLib promise closures, which run synchronously on the GLib thread while `run` (and thus `client`) is still alive. If a reviewer prefers, refactor `SignalingClient::send` onto a cloneable `out_tx` handle and move a clone into each closure instead — functionally identical, no `unsafe`. Pick one before committing.

- [ ] **Step 2: Replace the `unsafe` with a clean clone (do this now — preferred)**

Add to `engine/src/signaling.rs`:

```rust
impl SignalingClient {
    pub fn sender(&self) -> mpsc::UnboundedSender<ClientMessage> {
        self.out_tx.clone()
    }
}
```

Then in `peer.rs` replace each `let client_ptr = client as *const SignalingClient;` + `unsafe { &*client_ptr }` usage with a cloned `out_tx` sender captured by the closure, sending `tx.send(ClientMessage::…).ok();`. (Carry a `tx = client.sender()` into `set_target_and_offer` / the `Offer` arm.)

- [ ] **Step 3: Register module + CLI** — add `pub mod peer;` to `engine/src/lib.rs`, and extend `engine/src/main.rs`:

```rust
        "share" | "view" => {
            let share = mode == "share";
            let http = std::env::var("HEARTH_HTTP").unwrap_or("http://127.0.0.1:8080".into());
            let ws = std::env::var("HEARTH_WS").unwrap_or("ws://127.0.0.1:8080".into());
            let user = std::env::var("HEARTH_USER").expect("HEARTH_USER");
            let pass = std::env::var("HEARTH_PASS").expect("HEARTH_PASS");
            let room = std::env::var("HEARTH_ROOM").unwrap_or("main".into());

            let rt = tokio::runtime::Runtime::new()?;
            rt.block_on(engine::peer::run(&http, &ws, &user, &pass, &room, share))?;
        }
```

(Insert these arms into the existing `match mode.as_str()` before the `other =>` arm. `main` stays a plain `fn` returning `anyhow::Result<()>`; the runtime is created explicitly because GStreamer's GLib main loop runs on the main thread.)

- [ ] **Step 4: Build**

Run: `cd engine && cargo build`
Expected: compiles (no `unsafe` remaining after Step 2).

- [ ] **Step 5: Loopback run over the real backend (success criteria)**

Prepare (one-time): backend running + two users created.

```bash
# terminal 0 — backend
cd backend && DATABASE_URL=postgres://hearth:hearth@localhost:5433/hearth \
  JWT_SECRET=dev-only-change-me-min-32-bytes-long-secret cargo run &
# create two users via an admin (or seed directly); see backend admin endpoint.
```

```bash
# terminal 1 — viewer
cd engine
HEARTH_USER=bob HEARTH_PASS=pw-bob ./target/debug/engine view
# terminal 2 — sharer
HEARTH_USER=alice HEARTH_PASS=pw-alice ./target/debug/engine share
```

**Success criterion:** both processes print `connection-state: Connected`, the viewer prints `incoming stream linked -> displaying`, and a window shows the shared screen. This proves the full path **through the Hearth server** (no `/tmp` files). Record the result in a new `engine/README.md`.

- [ ] **Step 6: Commit**

```bash
git add engine
git commit -m "feat(engine): webrtcbin peer driven by the signaling server"
```

---

## Task 5: Cross-machine run + measurements (Windows ↔ Linux, user-run)

> This is the payoff of M4 and the originally-deferred R1/R4 validation. It needs a second machine; it is run by the user, not in this environment.

- [ ] **Step 1: Build the engine on the Windows box**

Prereqs on Windows: install Rust (MSVC toolchain) and the GStreamer **MSVC runtime + development** installers (1.24+, "complete" profile — includes `d3d11`, `webrtc`, `nice`). Set `PKG_CONFIG_PATH` / `GSTREAMER_1_0_ROOT_MSVC_X86_64` per the gstreamer-rs Windows docs. Then `cargo build` in `engine/`.

- [ ] **Step 2: Confirm Windows capture + AMF encode**

Run `engine probe` on Windows. **Expected:** `amfh265enc` selected (AMD), capture chain shows the `d3d11screencapturesrc` variant. If `d3d11screencapturesrc`/`d3d11download` names differ in the installed build, adjust `capture_chain()` for Windows and note it.

- [ ] **Step 3: Cross-machine call**

Point both engines at the same Hearth server (deploy it, or run on the Linux box and have the Windows box reach it on the LAN via `HEARTH_HTTP`/`HEARTH_WS`). Run `view` on one machine and `share` on the other. Test Windows→Linux and Linux→Windows.

- [ ] **Step 4: Record measurements** in `engine/README.md`
- glass-to-glass latency (phone-camera stopwatch); target < ~150 ms on LAN
- 1080p/60 legibility under motion (small-text readability, smearing)
- steady-state bitrate, CPU%, GPU encoder load on both ends
- whether direct ICE connects across the two real networks, or a TURN relay is needed (scopes coturn urgency for M6)

- [ ] **Step 5: Record the go/no-go** — GO confirms Approach A end-to-end on the real OS mix; otherwise escalate flow B to the Stage-2 dedicated transport (Spec §4). Commit the README updates.

---

## Self-Review (completed during authoring)

- **Spec coverage:** §2 boundaries — shared protocol crate, engine decoupled from backend except via protocol + WS (Tasks 1, 3). §3 signaling relay + auth-derived identity — engine logs in and connects with JWT, sends/receives typed messages (Task 3), peer targets ids from server messages (Task 4). §4 OBS-style encoder detection + Approach A transport — `encoders::detect` + `webrtcbin` over the server (Tasks 2, 4). §1 Windows+Linux MVP — per-OS `capture_chain` and AMF/VAAPI selection, verified Linux now and Windows in Task 5.
- **Placeholder scan:** no TBD/TODO; Tasks 1-3 are full TDD with code; Task 4 is complete code with a run-and-observe gate (justified — media path needs server+display); Task 5 is explicitly user-run hardware validation. The `unsafe` shortcut in Task 4 Step 1 is immediately removed in Step 2 with the concrete `sender()` clone approach.
- **Type consistency:** `ClientMessage`/`ServerMessage`/`PeerInfo` come from `hearth_protocol` everywhere (backend re-exports them; engine uses them directly). `SignalingClient::{connect, send, sender}`, `encoders::detect`, `capture::capture_chain`, and `peer::run` signatures match across their definitions and call sites. The protocol enums gain `Serialize+Deserialize` in Task 1, which both the backend (serializes Server / deserializes Client) and engine (the reverse) rely on.
- **Risk note:** Windows pipeline element names (`d3d11screencapturesrc`, `d3d11download`) are the most likely thing to need adjustment on real hardware; Task 5 Step 2 calls this out explicitly rather than assuming.
