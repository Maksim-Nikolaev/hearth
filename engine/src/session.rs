use crate::encoders;
use crate::flow::{Flow, VideoSink};
use crate::flow_peer::{
    build_screen_send_branch, build_voice_send_branch, link_video_recv, link_voice_recv,
};
use crate::signaling::{login, SignalingClient};
use anyhow::Result;
use gstreamer as gst;
use gstreamer::glib;
use gstreamer::prelude::*;
use gstreamer_sdp as gst_sdp;
use gstreamer_webrtc as gst_webrtc;
use hearth_protocol::{ChatEntry, ClientMessage, PeerInfo, ServerMessage};
use std::collections::HashMap;
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use uuid::Uuid;

#[derive(Debug, Clone)]
pub enum Presence {
    Roster(Vec<PeerInfo>),
    Joined { user: Uuid, username: String },
    Left { user: Uuid },
}

/// High-level events the UI consumes. Deliberately `Send` – the non-`Send`
/// video paintable is fetched separately via [`Session::paintable_for`] on the
/// main thread, so these may cross the relm4 command boundary.
#[derive(Debug, Clone)]
pub enum SessionEvent {
    Presence(Presence),
    Chat(ChatEntry),
    ChatHistory(Vec<ChatEntry>),
    FlowState { peer: Uuid, flow: Flow, state: String },
    VideoReady { peer: Uuid, flow: Flow },
    VoiceState(Vec<PeerInfo>),
    VoiceJoined { user: Uuid, username: String },
    VoiceLeft { user: Uuid },
    ShareStarted { user: Uuid },
    ShareStopped { user: Uuid },
    Error(String),
}

/// In a voice mesh both sides want to connect, so a deterministic rule decides
/// who offers: the peer with the smaller `Uuid`. The other side answers the
/// incoming offer. (Screenshare is directional – the sharer always offers.)
pub(crate) fn should_offer(me: Uuid, peer: Uuid) -> bool {
    me < peer
}

/// How a local screenshare reaches viewers. P2P now (one offerer flow per
/// viewer); a future SFU impl negotiates once with the backend instead, without
/// changing the UI or `Session::start_share`.
pub trait ScreenTransport {
    /// Begin sharing to the given current voice members.
    fn start(&mut self, session: &mut Session, viewers: &[Uuid]);

    /// Stop all local screenshare flows.
    fn stop(&mut self, session: &mut Session);
}

/// P2P fan-out: the sharer opens one offerer Screen flow per viewer.
pub struct P2pTransport;

impl ScreenTransport for P2pTransport {
    fn start(&mut self, session: &mut Session, viewers: &[Uuid]) {
        for &v in viewers {
            if let Err(e) = session.start_offerer(v, Flow::Screen) {
                session.emit(SessionEvent::Error(format!("screen offer: {e}")));
            }
        }
    }

    fn stop(&mut self, session: &mut Session) {
        let screens: Vec<Uuid> = session
            .peers
            .keys()
            .filter(|(_, f)| *f == Flow::Screen)
            .map(|(p, _)| *p)
            .collect();

        for p in screens {
            session.stop_flow(p, Flow::Screen);
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Role {
    Offerer,
    Answerer,
}

/// One media flow to one peer, carried by a single `webrtcbin`. Non-blocking:
/// it attaches to the ambient GLib main context (GTK's loop), never its own.
pub(crate) struct FlowPeer {
    pipeline: gst::Pipeline,
    webrtc: gst::Element,
    flow: Flow,
    target: Uuid,
    out_tx: UnboundedSender<ClientMessage>,
    paintable: Option<glib::Object>,
}

impl FlowPeer {
    fn new(
        flow: Flow,
        role: Role,
        target: Uuid,
        sink: VideoSink,
        out_tx: UnboundedSender<ClientMessage>,
        evt_tx: UnboundedSender<SessionEvent>,
    ) -> Result<Self> {
        gst::init()?;

        let pipeline = gst::Pipeline::new();
        let webrtc = gst::ElementFactory::make("webrtcbin")
            .name("wrtc")
            .property_from_str("stun-server", "stun://stun.l.google.com:19302")
            .build()?;

        if let Ok(turn) = std::env::var("HEARTH_TURN") {
            if !turn.trim().is_empty() {
                webrtc.set_property_from_str("turn-server", &turn);
            }
        }

        pipeline.add(&webrtc)?;

        // Bus errors/warnings -> events.
        let bus = pipeline.bus().expect("pipeline has a bus");
        let evt_bus = evt_tx.clone();
        let _bus_watch = bus.add_watch(move |_, msg| {
            use gst::MessageView;
            if let MessageView::Error(e) = msg.view() {
                let _ = evt_bus.send(SessionEvent::Error(format!("{} ({:?})", e.error(), e.debug())));
            }
            glib::ControlFlow::Continue
        })?;
        std::mem::forget(_bus_watch); // keep the watch alive for the pipeline's lifetime

        // Send branch: voice is bidirectional; screenshare flows offerer -> answerer.
        let do_send = matches!(flow, Flow::Voice) || matches!(role, Role::Offerer);
        if do_send {
            match flow {
                Flow::Screen => {
                    let encoder = encoders::detect().0.unwrap_or("x265enc");
                    build_screen_send_branch(&pipeline, &webrtc, encoder)?;
                }
                Flow::Voice => build_voice_send_branch(&pipeline, &webrtc)?,
                Flow::Webcam => anyhow::bail!("webcam flow is out of M5 scope"),
            }
        }

        // Pre-create the video display sink for the viewer (screen answerer only),
        // reading the paintable on this (main) thread.
        let mut paintable = None;
        let video_sink = if flow != Flow::Voice && matches!(role, Role::Answerer) {
            let s = match sink {
                VideoSink::Auto => gst::ElementFactory::make("autovideosink")
                    .property("sync", false)
                    .build()?,
                VideoSink::Paintable => {
                    let s = gst::ElementFactory::make("gtk4paintablesink").build()?;
                    paintable = Some(s.property::<glib::Object>("paintable"));
                    s
                }
            };
            Some(std::sync::Arc::new(s))
        } else {
            None
        };

        let pipeline_weak = pipeline.downgrade();
        let vsink = video_sink.clone();
        webrtc.connect_pad_added(move |_w, pad| {
            if pad.direction() != gst::PadDirection::Src {
                return;
            }
            let Some(pipeline) = pipeline_weak.upgrade() else {
                return;
            };
            match flow {
                Flow::Voice => link_voice_recv(&pipeline, pad),
                _ => {
                    if let Some(vsink) = vsink.as_ref() {
                        link_video_recv(&pipeline, pad, vsink);
                    }
                }
            }
        });

        // Local ICE -> signaling (target known up-front, no buffering).
        {
            let out = out_tx.clone();
            webrtc.connect("on-ice-candidate", false, move |vals| {
                let mline = vals[1].get::<u32>().unwrap();
                let cand = vals[2].get::<String>().unwrap();
                let _ = out.send(ClientMessage::Ice { to: target, flow, mline, candidate: cand });
                None
            });
        }

        // Connection state -> events.
        {
            let evt = evt_tx.clone();
            webrtc.connect_notify(Some("connection-state"), move |w, _| {
                let s = w.property::<gst_webrtc::WebRTCPeerConnectionState>("connection-state");
                let _ = evt.send(SessionEvent::FlowState { peer: target, flow, state: format!("{s:?}") });
            });
        }

        pipeline.set_state(gst::State::Playing)?;

        // Offerer kicks off negotiation immediately (target is known).
        if matches!(role, Role::Offerer) {
            let w = webrtc.clone();
            let out = out_tx.clone();
            let promise = gst::Promise::with_change_func(move |reply| {
                let Ok(Some(reply)) = reply else { return };
                let offer = reply.value("offer").unwrap().get::<gst_webrtc::WebRTCSessionDescription>().unwrap();
                w.emit_by_name::<()>("set-local-description", &[&offer, &None::<gst::Promise>]);
                let _ = out.send(ClientMessage::Offer { to: target, flow, sdp: offer.sdp().as_text().unwrap().to_string() });
            });
            webrtc.emit_by_name::<()>("create-offer", &[&None::<gst::Structure>, &promise]);
        }

        if paintable.is_some() {
            let _ = evt_tx.send(SessionEvent::VideoReady { peer: target, flow });
        }

        Ok(Self { pipeline, webrtc, flow, target, out_tx, paintable })
    }

    fn handle_offer(&self, sdp: &str) {
        let Ok(sdp) = gst_sdp::SDPMessage::parse_buffer(sdp.as_bytes()) else { return };
        let offer = gst_webrtc::WebRTCSessionDescription::new(gst_webrtc::WebRTCSDPType::Offer, sdp);
        self.webrtc.emit_by_name::<()>("set-remote-description", &[&offer, &None::<gst::Promise>]);

        let w = self.webrtc.clone();
        let to = self.target;
        let flow = self.flow;
        let out = self.out_tx.clone();
        let promise = gst::Promise::with_change_func(move |reply| {
            let Ok(Some(reply)) = reply else { return };
            let answer = reply.value("answer").unwrap().get::<gst_webrtc::WebRTCSessionDescription>().unwrap();
            w.emit_by_name::<()>("set-local-description", &[&answer, &None::<gst::Promise>]);
            let _ = out.send(ClientMessage::Answer { to, flow, sdp: answer.sdp().as_text().unwrap().to_string() });
        });
        self.webrtc.emit_by_name::<()>("create-answer", &[&None::<gst::Structure>, &promise]);
    }

    fn handle_answer(&self, sdp: &str) {
        let Ok(sdp) = gst_sdp::SDPMessage::parse_buffer(sdp.as_bytes()) else { return };
        let answer = gst_webrtc::WebRTCSessionDescription::new(gst_webrtc::WebRTCSDPType::Answer, sdp);
        self.webrtc.emit_by_name::<()>("set-remote-description", &[&answer, &None::<gst::Promise>]);
    }

    fn add_ice(&self, mline: u32, candidate: &str) {
        self.webrtc.emit_by_name::<()>("add-ice-candidate", &[&mline, &candidate.to_string()]);
    }

    fn set_valve(&self, name: &str, drop: bool) {
        if let Some(v) = self.pipeline.by_name(name) {
            v.set_property("drop", drop);
        }
    }

    fn stop(&self) {
        let _ = self.pipeline.set_state(gst::State::Null);
    }
}

/// Owns the control-plane WebSocket and the per-(peer, flow) `FlowPeer` registry.
/// All methods run on the GTK main thread; the app pumps inbound `ServerMessage`s
/// into [`Session::handle`] there.
pub struct Session {
    out_tx: UnboundedSender<ClientMessage>,
    evt_tx: UnboundedSender<SessionEvent>,
    sink: VideoSink,
    /// The logged-in user, decoded from the JWT `sub` claim. Drives the voice
    /// offerer rule (smaller `Uuid` offers).
    self_id: Uuid,
    /// Current Voice channel members (excluding self), kept in sync from voice
    /// events. The screenshare fan-out targets exactly this set.
    voice_members: Vec<Uuid>,
    screen_transport: Box<dyn ScreenTransport>,
    pub(crate) peers: HashMap<(Uuid, Flow), FlowPeer>,
    _client: Option<SignalingClient>,
}

/// Read the user id from a JWT's `sub` claim without verifying the signature:
/// the server already authenticated us, this only needs our own identity.
fn self_id_from_token(token: &str) -> Option<Uuid> {
    use base64::Engine;

    let payload = token.split('.').nth(1)?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(payload).ok()?;
    let claims: serde_json::Value = serde_json::from_slice(&bytes).ok()?;

    claims.get("sub")?.as_str()?.parse().ok()
}

/// The `Send` result of opening a connection: handed back from an async task,
/// then turned into a (non-`Send`) [`Session`] on the main thread via
/// [`Session::start`].
pub struct Connection {
    client: SignalingClient,
    inbound: UnboundedReceiver<ServerMessage>,
    token: String,
}

impl Connection {
    /// The access token, for persisting so a later launch can skip the password.
    pub fn token(&self) -> &str {
        &self.token
    }
}

impl Session {
    /// Log in (username/password) and open the WebSocket. Async + `Send`, so it
    /// runs in a background task.
    pub async fn open(http_base: &str, ws_base: &str, username: &str, password: &str) -> Result<Connection> {
        let token = login(http_base, username, password).await?;
        Self::open_with_token(ws_base, &token).await
    }

    /// Open the WebSocket with an existing token (skips the password). Errors if
    /// the token is rejected, so the caller can fall back to a fresh login.
    pub async fn open_with_token(ws_base: &str, token: &str) -> Result<Connection> {
        let (client, inbound) = SignalingClient::connect(ws_base, token).await?;

        Ok(Connection { client, inbound, token: token.to_string() })
    }

    /// Build the session on the main thread, returning the inbound `ServerMessage`
    /// stream (pump into [`Session::handle`] on the main thread) and the
    /// high-level `SessionEvent` stream (drive the UI).
    pub fn start(
        conn: Connection,
        sink: VideoSink,
    ) -> (Self, UnboundedReceiver<ServerMessage>, UnboundedReceiver<SessionEvent>) {
        let _ = gst::init();

        let out_tx = conn.client.sender();
        let (evt_tx, evt_rx) = mpsc::unbounded_channel();
        let self_id = self_id_from_token(&conn.token).unwrap_or_default();

        let session = Session {
            out_tx,
            evt_tx,
            sink,
            self_id,
            voice_members: Vec::new(),
            screen_transport: Box::new(P2pTransport),
            peers: HashMap::new(),
            _client: Some(conn.client),
        };

        (session, conn.inbound, evt_rx)
    }

    pub fn join(&self, room: &str) {
        let _ = self.out_tx.send(ClientMessage::Join { room: room.to_string() });
    }

    pub fn send_chat(&self, body: &str) {
        let _ = self.out_tx.send(ClientMessage::Chat { body: body.to_string() });
    }

    /// Join the Voice channel. The mesh is built reactively from the resulting
    /// `VoiceState`/`VoiceJoined` server messages.
    pub fn join_voice(&self) {
        let _ = self.out_tx.send(ClientMessage::VoiceJoin);
    }

    /// Leave Voice: tear down every Voice flow, then tell the server.
    pub fn leave_voice(&mut self) {
        let voice_peers: Vec<Uuid> = self
            .peers
            .keys()
            .filter(|(_, f)| *f == Flow::Voice)
            .map(|(p, _)| *p)
            .collect();

        for p in voice_peers {
            self.stop_flow(p, Flow::Voice);
        }

        self.voice_members.clear();

        let _ = self.out_tx.send(ClientMessage::VoiceLeave);
    }

    /// Open a Voice flow to one mesh member, offering only if our `Uuid` is the
    /// smaller one; otherwise we wait for their offer (answered in `handle`).
    fn connect_voice(&mut self, peer: Uuid) {
        if peer == self.self_id {
            return;
        }

        if !self.voice_members.contains(&peer) {
            self.voice_members.push(peer);
        }

        if self.peers.contains_key(&(peer, Flow::Voice)) {
            return;
        }

        if should_offer(self.self_id, peer) {
            if let Err(e) = self.start_offerer(peer, Flow::Voice) {
                self.emit(SessionEvent::Error(format!("voice offer: {e}")));
            }
        }
    }

    /// Start sharing your screen to every current voice member, via the active
    /// `ScreenTransport` (P2P now). Also tells the server so others list you.
    pub fn start_share(&mut self) {
        let _ = self.out_tx.send(ClientMessage::ShareStart);

        let viewers = self.voice_members.clone();
        let mut t = std::mem::replace(&mut self.screen_transport, Box::new(P2pTransport));
        t.start(self, &viewers);
        self.screen_transport = t;
    }

    /// Stop sharing: tear down the local screenshare flows and notify the server.
    pub fn stop_share(&mut self) {
        let _ = self.out_tx.send(ClientMessage::ShareStop);

        let mut t = std::mem::replace(&mut self.screen_transport, Box::new(P2pTransport));
        t.stop(self);
        self.screen_transport = t;
    }

    pub fn start_call(&mut self, peer: Uuid) -> Result<()> {
        self.start_offerer(peer, Flow::Voice)
    }

    pub(crate) fn start_offerer(&mut self, peer: Uuid, flow: Flow) -> Result<()> {
        let key = (peer, flow);
        if self.peers.contains_key(&key) {
            return Ok(());
        }
        let p = FlowPeer::new(flow, Role::Offerer, peer, self.sink, self.out_tx.clone(), self.evt_tx.clone())?;
        self.peers.insert(key, p);

        Ok(())
    }

    pub fn stop_flow(&mut self, peer: Uuid, flow: Flow) {
        if let Some(p) = self.peers.remove(&(peer, flow)) {
            p.stop();
        }
    }

    /// Tear down every active media flow (the other peers and chat are untouched).
    pub fn stop_all(&mut self) {
        for (_, p) in self.peers.drain() {
            p.stop();
        }
    }

    pub fn mute(&self, on: bool) {
        for p in self.peers.values() {
            if p.flow == Flow::Voice {
                p.set_valve("mic_valve", on);
            }
        }
    }

    pub fn deafen(&self, on: bool) {
        for p in self.peers.values() {
            if p.flow == Flow::Voice {
                p.set_valve("spk_valve", on);
                p.set_valve("mic_valve", on);
            }
        }
    }

    /// The incoming video paintable for a flow, fetched on the main thread.
    pub fn paintable_for(&self, peer: Uuid, flow: Flow) -> Option<glib::Object> {
        self.peers.get(&(peer, flow)).and_then(|p| p.paintable.clone())
    }

    /// Route one inbound server message: presence/chat become events; signaling
    /// drives the matching `FlowPeer` (creating an answerer on a fresh offer).
    pub fn handle(&mut self, msg: ServerMessage) {
        match msg {
            ServerMessage::RoomPeers { peers } => self.emit(SessionEvent::Presence(Presence::Roster(peers))),
            ServerMessage::PeerJoined { user, username } => {
                self.emit(SessionEvent::Presence(Presence::Joined { user, username }))
            }
            ServerMessage::PeerLeft { user } => self.emit(SessionEvent::Presence(Presence::Left { user })),
            ServerMessage::Chat { from, username, body, at } => {
                self.emit(SessionEvent::Chat(ChatEntry { from, username, body, at }))
            }
            ServerMessage::ChatHistory { messages } => self.emit(SessionEvent::ChatHistory(messages)),
            ServerMessage::Offer { from, flow, sdp } => {
                let key = (from, flow);

                // A fresh offer starts a new session for this flow. Drop any stale
                // peer first so re-sharing after a Stop renegotiates cleanly (and
                // releases the old webrtcbin's ICE port).
                if let Some(old) = self.peers.remove(&key) {
                    old.stop();
                }

                match FlowPeer::new(flow, Role::Answerer, from, self.sink, self.out_tx.clone(), self.evt_tx.clone()) {
                    Ok(p) => {
                        p.handle_offer(&sdp);
                        self.peers.insert(key, p);
                    }
                    Err(e) => self.emit(SessionEvent::Error(format!("create answerer: {e}"))),
                }
            }
            ServerMessage::Answer { from, flow, sdp } => {
                if let Some(p) = self.peers.get(&(from, flow)) {
                    p.handle_answer(&sdp);
                }
            }
            ServerMessage::Ice { from, flow, mline, candidate } => {
                if let Some(p) = self.peers.get(&(from, flow)) {
                    p.add_ice(mline, &candidate);
                }
            }
            ServerMessage::VoiceState { members } => {
                for m in &members {
                    self.connect_voice(m.user);
                }
                self.emit(SessionEvent::VoiceState(members));
            }
            ServerMessage::VoiceJoined { user, username } => {
                self.connect_voice(user);
                self.emit(SessionEvent::VoiceJoined { user, username });
            }
            ServerMessage::VoiceLeft { user } => {
                self.voice_members.retain(|m| *m != user);
                self.stop_flow(user, Flow::Voice);
                self.emit(SessionEvent::VoiceLeft { user });
            }
            ServerMessage::ShareStarted { user } => self.emit(SessionEvent::ShareStarted { user }),
            ServerMessage::ShareStopped { user } => self.emit(SessionEvent::ShareStopped { user }),
        }
    }

    pub(crate) fn emit(&self, evt: SessionEvent) {
        let _ = self.evt_tx.send(evt);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    impl Session {
        fn for_test() -> (Session, UnboundedReceiver<SessionEvent>) {
            let (out_tx, _out_rx) = mpsc::unbounded_channel();
            let (evt_tx, evt_rx) = mpsc::unbounded_channel();
            let s = Session {
                out_tx,
                evt_tx,
                sink: VideoSink::Auto,
                self_id: Uuid::nil(),
                voice_members: Vec::new(),
                screen_transport: Box::new(P2pTransport),
                peers: HashMap::new(),
                _client: None,
            };
            (s, evt_rx)
        }
    }

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

    #[test]
    fn routes_chat_to_event() {
        let (mut s, mut evt_rx) = Session::for_test();

        s.handle(ServerMessage::Chat { from: Uuid::now_v7(), username: "a".into(), body: "hi".into(), at: 1 });

        match evt_rx.try_recv().unwrap() {
            SessionEvent::Chat(e) => assert_eq!(e.body, "hi"),
            other => panic!("expected Chat, got {other:?}"),
        }
    }

    #[test]
    fn routes_presence_roster_to_event() {
        let (mut s, mut evt_rx) = Session::for_test();

        s.handle(ServerMessage::RoomPeers { peers: vec![] });

        match evt_rx.try_recv().unwrap() {
            SessionEvent::Presence(Presence::Roster(p)) => assert!(p.is_empty()),
            other => panic!("expected Presence::Roster, got {other:?}"),
        }
    }

    #[test]
    fn routes_chat_history_to_event() {
        let (mut s, mut evt_rx) = Session::for_test();

        let entry = ChatEntry { from: Uuid::now_v7(), username: "a".into(), body: "old".into(), at: 1 };
        s.handle(ServerMessage::ChatHistory { messages: vec![entry] });

        match evt_rx.try_recv().unwrap() {
            SessionEvent::ChatHistory(m) => assert_eq!(m.len(), 1),
            other => panic!("expected ChatHistory, got {other:?}"),
        }
    }
}
