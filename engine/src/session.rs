use crate::audio::capture::VoiceCapture;
use crate::audio::dsp::{DspConfig, NsLevel};
use crate::audio::monitor::Monitor;
use crate::audio::gate::{ActivationMode, Gate};
use crate::hotkey::{keysym_from_name, PttGrab};
use crate::flow::{Flow, VideoSink};
use crate::flow_peer::{
    build_screen_send_appsrc_branch, build_voice_send_branch, link_screen_audio_recv,
    link_video_recv, link_voice_recv,
};
use crate::screen::{self, ShareConfig};
use crate::signaling::{login, SignalingClient};
use anyhow::Result;
use gstreamer as gst;
use gstreamer::glib;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use gstreamer_sdp as gst_sdp;
use gstreamer_webrtc as gst_webrtc;
use hearth_protocol::{ChatEntry, ClientMessage, PeerInfo, ServerMessage};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
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
    /// Local microphone level in RMS dBFS, emitted ~once per processed frame for
    /// the input meter. The capture already feeds the gate; the UI only displays.
    InputLevel(f32),
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
    /// For Voice send flows: the `appsrc` the shared `VoiceCapture` pushes the
    /// DSP'd mic frame into. `None` for receive-only / non-voice flows.
    voice_appsrc: Option<gst_app::AppSrc>,
    /// For Screen offerer flows: the `appsrc` that `ScreenSource` fans encoded
    /// H265 buffers into. `None` for answerer / non-screen flows.
    screen_appsrc: Option<gst_app::AppSrc>,
    /// Gates the shared `ScreenSource` fan-out into this peer's screen send
    /// branch. Set true once `webrtcbin` reaches `Connected`; the encoder feeds
    /// the branch only then, because pushing encoded H265 into a not-yet-negotiated
    /// `webrtcbin` wedges `rtph265pay` (its first caps push blocks) and no video
    /// RTP ever flows – the connection comes up but the viewer stays black.
    screen_ready: Arc<AtomicBool>,
}

impl FlowPeer {
    fn new(
        flow: Flow,
        role: Role,
        target: Uuid,
        sink: VideoSink,
        out_tx: UnboundedSender<ClientMessage>,
        evt_tx: UnboundedSender<SessionEvent>,
        // Capture chain string; kept for the legacy `flow_peer::run()` path.
        // Session-managed Screen flows always pass `None` – the shared
        // `ScreenSource` handles capture centrally.
        _screen_chain: Option<String>,
        // Optional audio source chain for the screen flow (payload 98). `None`
        // means no audio track is added (video-only share, M6 default).
        screen_audio: Option<String>,
        // Encoder bitrate in kbps; overridden by HEARTH_BITRATE_KBPS env var.
        _bitrate_kbps: u32,
    ) -> Result<Self> {
        gst::init()?;

        let pipeline = gst::Pipeline::new();
        let webrtc = gst::ElementFactory::make("webrtcbin")
            .name("wrtc")
            .property_from_str("stun-server", "stun://stun.l.google.com:19302")
            // 40 ms jitter buffer (vs webrtcbin's 200 ms default) — the single
            // biggest cut to end-to-end audio/video latency. Tunable via
            // HEARTH_JITTER_MS. See crate::flow_peer::jitter_latency_ms.
            .property("latency", crate::flow_peer::jitter_latency_ms())
            .build()?;

        if let Ok(turn) = std::env::var("HEARTH_TURN") {
            if !turn.trim().is_empty() {
                webrtc.set_property_from_str("turn-server", &turn);
            }
        }

        pipeline.add(&webrtc)?;

        // Bus errors -> events; warnings are logged. Both are also printed so a
        // silent stall (e.g. a mis-negotiated send branch) leaves a trace.
        let bus = pipeline.bus().expect("pipeline has a bus");
        let evt_bus = evt_tx.clone();
        let _bus_watch = bus.add_watch(move |_, msg| {
            use gst::MessageView;
            match msg.view() {
                MessageView::Error(e) => {
                    let detail = format!("{} ({:?})", e.error(), e.debug());
                    eprintln!("{flow:?} pipeline error from {:?}: {detail}", e.src().map(|s| s.path_string()));
                    let _ = evt_bus.send(SessionEvent::Error(detail));
                }
                MessageView::Warning(w) => {
                    eprintln!(
                        "{flow:?} pipeline warning from {:?}: {} ({:?})",
                        w.src().map(|s| s.path_string()),
                        w.error(),
                        w.debug()
                    );
                }
                _ => {}
            }
            glib::ControlFlow::Continue
        })?;
        std::mem::forget(_bus_watch); // keep the watch alive for the pipeline's lifetime

        // Send branch: voice is bidirectional; screenshare flows offerer -> answerer.
        let do_send = matches!(flow, Flow::Voice) || matches!(role, Role::Offerer);
        let mut voice_appsrc = None;
        let mut screen_appsrc = None;
        if do_send {
            match flow {
                Flow::Screen => {
                    screen_appsrc = Some(build_screen_send_appsrc_branch(
                        &pipeline,
                        &webrtc,
                        screen_audio.as_deref(),
                    )?);
                }
                Flow::Voice => voice_appsrc = Some(build_voice_send_branch(&pipeline, &webrtc)?),
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
                    // sync=false: render each frame on arrival rather than
                    // clock-syncing to its PTS. For a live screen view that keeps
                    // latency low and avoids dropping frames the sink judges
                    // "too late" (matches the autovideosink path above).
                    let s = gst::ElementFactory::make("gtk4paintablesink")
                        .property("sync", false)
                        .build()?;
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
                Flow::Screen => {
                    // Distinguish the optional audio track (payload 98) from the
                    // video track by the RTP caps: prefer the `media` field, fall
                    // back to the codec (`encoding-name` OPUS) so a missing media
                    // field never misroutes the video pad into the audio sink.
                    let is_audio = pad
                        .current_caps()
                        .as_ref()
                        .and_then(|c| c.structure(0).map(|s| s.to_owned()))
                        .map(|s| {
                            s.get::<&str>("media").map(|m| m == "audio").unwrap_or(false)
                                || s.get::<&str>("encoding-name")
                                    .map(|e| e.eq_ignore_ascii_case("OPUS"))
                                    .unwrap_or(false)
                        })
                        .unwrap_or(false);

                    if is_audio {
                        link_screen_audio_recv(&pipeline, pad);
                    } else if let Some(vsink) = vsink.as_ref() {
                        link_video_recv(&pipeline, pad, vsink);
                    }
                }
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

        // Gates the shared encoder's fan-out into this peer's screen send branch;
        // flipped true by the connection-state handler once the peer is connected.
        let screen_ready = Arc::new(AtomicBool::new(false));

        // Connection state -> events. For Screen answerer flows, a terminal
        // state (Failed/Disconnected/Closed) also signals ShareStopped so the
        // viewer stage clears even when signaling lags behind media-level drop.
        // For Screen offerer flows, reaching Connected opens the fan-out gate.
        {
            let evt = evt_tx.clone();
            let is_screen_viewer = flow == Flow::Screen && matches!(role, Role::Answerer);
            let is_screen_sharer = flow == Flow::Screen && matches!(role, Role::Offerer);
            let ready = screen_ready.clone();
            webrtc.connect_notify(Some("connection-state"), move |w, _| {
                let s = w.property::<gst_webrtc::WebRTCPeerConnectionState>("connection-state");
                let _ = evt.send(SessionEvent::FlowState { peer: target, flow, state: format!("{s:?}") });

                use gst_webrtc::WebRTCPeerConnectionState as St;

                // Open the gate only once the connection is up: feeding encoded
                // H265 into a not-yet-negotiated webrtcbin wedges rtph265pay.
                if is_screen_sharer && matches!(s, St::Connected) {
                    ready.store(true, Ordering::Relaxed);
                }

                if is_screen_viewer && matches!(s, St::Failed | St::Disconnected | St::Closed) {
                    let _ = evt.send(SessionEvent::ShareStopped { user: target });
                }
            });
        }

        pipeline.set_state(gst::State::Playing)?;

        // Negotiation is kicked off by the caller via `start_negotiation()` once
        // the send branch is wired. The offer's codec caps come from each branch's
        // capsfilter, so it is complete regardless of whether media is flowing yet.

        if paintable.is_some() {
            let _ = evt_tx.send(SessionEvent::VideoReady { peer: target, flow });
        }

        Ok(Self { pipeline, webrtc, flow, target, out_tx, paintable, voice_appsrc, screen_appsrc, screen_ready })
    }

    /// Create and send the SDP offer. Call on offerer flows only. The offer's
    /// codec caps come from each send branch's capsfilter, so it is complete even
    /// before any media has flowed.
    pub(crate) fn start_negotiation(&self) {
        let w = self.webrtc.clone();
        let out = self.out_tx.clone();
        let target = self.target;
        let flow = self.flow;
        let promise = gst::Promise::with_change_func(move |reply| {
            let Ok(Some(reply)) = reply else { return };
            let offer = reply.value("offer").unwrap().get::<gst_webrtc::WebRTCSessionDescription>().unwrap();
            let sdp = offer.sdp().as_text().unwrap_or_default();
            w.emit_by_name::<()>("set-local-description", &[&offer, &None::<gst::Promise>]);
            let _ = out.send(ClientMessage::Offer { to: target, flow, sdp });
        });
        self.webrtc.emit_by_name::<()>("create-offer", &[&None::<gst::Structure>, &promise]);
    }

    /// The voice send `appsrc` for this flow, if it is a Voice send branch.
    fn voice_appsrc(&self) -> Option<gst_app::AppSrc> {
        self.voice_appsrc.clone()
    }

    /// The screen send `appsrc` for this flow, if it is a Screen offerer branch.
    pub(crate) fn screen_appsrc(&self) -> Option<gst_app::AppSrc> {
        self.screen_appsrc.clone()
    }

    /// The fan-out readiness gate for this peer's screen send branch; passed to
    /// `ScreenSource::register_viewer` so the encoder waits for `Connected`.
    pub(crate) fn screen_ready(&self) -> Arc<AtomicBool> {
        self.screen_ready.clone()
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
            let asdp = answer.sdp().as_text().unwrap_or_default();
            w.emit_by_name::<()>("set-local-description", &[&answer, &None::<gst::Promise>]);
            let _ = out.send(ClientMessage::Answer { to, flow, sdp: asdp });
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
    /// The logged-in username, decoded from the JWT `username` claim. Shown in
    /// the self-panel and used to mark "(you)" in member lists.
    self_name: String,
    /// Current Voice channel members (excluding self), kept in sync from voice
    /// events. The screenshare fan-out targets exactly this set.
    voice_members: Vec<Uuid>,
    screen_transport: Box<dyn ScreenTransport>,
    /// Config supplied to the most recent `start_share`. Used to build the
    /// capture+caps chain for each Screen offerer flow.
    share_config: ShareConfig,
    /// Screen (and webcam) flows, each a `webrtcbin`. Voice no longer lives here
    /// — it uses the low-latency UDP transport in `voice_peers`.
    pub(crate) peers: HashMap<(Uuid, Flow), FlowPeer>,
    /// Voice flows, one thin RTP/Opus-over-UDP transport per peer (no webrtcbin).
    voice_peers: HashMap<Uuid, crate::voice_udp::VoiceUdpPeer>,
    /// The single mic capture + DSP, shared by every Voice flow. Lazily started
    /// when the first voice peer connects, dropped when the last leaves.
    voice_capture: Option<VoiceCapture>,
    /// Standalone loopback monitor for the Settings mic-test. Runs only when
    /// NOT in a call; `start_mic_test`/`stop_mic_test` gate it.
    mic_monitor: Option<Monitor>,
    /// Software activation gate, shared with the capture callback thread.
    gate: Arc<Mutex<Gate>>,
    dsp_config: DspConfig,
    input_device: Option<String>,
    output_device: Option<String>,
    /// Active X11 global key grab for push-to-talk. Dropped (ungrab + thread
    /// join) whenever the PTT key is cleared or changed.
    ptt_grab: Option<PttGrab>,
    _client: Option<SignalingClient>,
    /// Single capture+encode+preview pipeline. Active during both preview-only
    /// (`encode == false`) and live share (`encode == true`). `None` when idle
    /// or when the elements are unavailable (e.g. headless test environments).
    screen_source: Option<screen::ScreenSource>,
}

/// Sensible defaults: AEC + high-pass + moderate NS + AGC + VAD on.
fn default_dsp_config() -> DspConfig {
    DspConfig {
        echo_cancel: true,
        noise_suppression: NsLevel::Moderate,
        agc: true,
        vad: true,
        high_pass: true,
    }
}

/// Read our identity (`sub`, `username`) from a JWT without verifying the
/// signature: the server already authenticated us, this only needs our own id.
fn self_from_token(token: &str) -> (Uuid, String) {
    use base64::Engine;

    let claims: Option<serde_json::Value> = token
        .split('.')
        .nth(1)
        .and_then(|p| base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(p).ok())
        .and_then(|b| serde_json::from_slice(&b).ok());

    let Some(claims) = claims else {
        return (Uuid::nil(), String::new());
    };

    let id = claims.get("sub").and_then(|v| v.as_str()).and_then(|s| s.parse().ok()).unwrap_or_default();
    let name = claims.get("username").and_then(|v| v.as_str()).unwrap_or_default().to_string();

    (id, name)
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
        let (self_id, self_name) = self_from_token(&conn.token);

        let session = Session {
            out_tx,
            evt_tx,
            sink,
            self_id,
            self_name,
            voice_members: Vec::new(),
            screen_transport: Box::new(P2pTransport),
            share_config: ShareConfig::default(),
            peers: HashMap::new(),
            voice_peers: HashMap::new(),
            voice_capture: None,
            mic_monitor: None,
            gate: Arc::new(Mutex::new(Gate::new(ActivationMode::Voice { threshold: -45.0 }))),
            dsp_config: default_dsp_config(),
            input_device: None,
            output_device: None,
            ptt_grab: None,
            _client: Some(conn.client),
            screen_source: None,
        };

        (session, conn.inbound, evt_rx)
    }

    /// The logged-in user's id (from the JWT).
    pub fn self_id(&self) -> Uuid {
        self.self_id
    }

    /// The logged-in user's display name (from the JWT).
    pub fn self_name(&self) -> &str {
        &self.self_name
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
        // Leaving the call disconnects every media flow — voice AND any
        // screenshare being watched (Discord-like). Chat/presence are untouched.
        self.voice_peers.clear();
        self.voice_capture = None;

        let screen: Vec<(Uuid, Flow)> = self.peers.keys().copied().collect();
        for (peer, flow) in screen {
            // Clear the stage for a screenshare we were watching.
            if flow == Flow::Screen {
                self.emit(SessionEvent::ShareStopped { user: peer });
            }
            self.stop_flow(peer, flow);
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

        if self.voice_peers.contains_key(&peer) {
            return;
        }

        if should_offer(self.self_id, peer) {
            if let Err(e) = self.voice_offer(peer) {
                self.emit(SessionEvent::Error(format!("voice offer: {e}")));
            }
        }
    }

    /// Offerer side of a UDP voice flow: build the transport, register the mic
    /// send, and hand the peer our `ip:port` (carried in the Offer's `sdp`).
    fn voice_offer(&mut self, peer: Uuid) -> Result<()> {
        let p = crate::voice_udp::VoiceUdpPeer::new(peer)?;
        let endpoint = p.local_endpoint();
        self.voice_peers.insert(peer, p);
        self.register_voice_send(peer);
        let _ = self.out_tx.send(ClientMessage::Offer {
            to: peer,
            flow: Flow::Voice,
            sdp: endpoint,
        });
        Ok(())
    }

    /// Answerer side: the peer sent their endpoint; build our transport pointed
    /// at them and reply with ours.
    fn voice_on_offer(&mut self, from: Uuid, endpoint: &str) {
        self.stop_voice(from); // a re-offer replaces any stale transport
        match crate::voice_udp::VoiceUdpPeer::new(from) {
            Ok(p) => {
                if let Err(e) = p.set_remote(endpoint) {
                    self.emit(SessionEvent::Error(format!("voice endpoint: {e}")));
                }
                let my_endpoint = p.local_endpoint();
                self.voice_peers.insert(from, p);
                self.register_voice_send(from);
                let _ = self.out_tx.send(ClientMessage::Answer {
                    to: from,
                    flow: Flow::Voice,
                    sdp: my_endpoint,
                });
            },
            Err(e) => self.emit(SessionEvent::Error(format!("voice transport: {e}"))),
        }
    }

    /// Offerer received the answer: point our sender at the peer.
    fn voice_on_answer(&mut self, from: Uuid, endpoint: &str) {
        if let Some(p) = self.voice_peers.get(&from) {
            if let Err(e) = p.set_remote(endpoint) {
                self.emit(SessionEvent::Error(format!("voice endpoint: {e}")));
            }
        }
    }

    /// Tear down one voice peer and unregister its mic send.
    fn stop_voice(&mut self, peer: Uuid) {
        if let Some(p) = self.voice_peers.remove(&peer) {
            let appsrc = Some(p.voice_appsrc());
            drop(p);
            self.unregister_voice_send(appsrc);
        }
    }

    /// Rebuild the active voice transports so a new jitter-buffer depth takes
    /// effect (`rtpjitterbuffer` only honours `latency` at construction). Touches
    /// only voice — screenshare is left running. The caller re-offers to every
    /// voice peer regardless of the usual offerer election, so the rebuild works
    /// from either side; each peer's `voice_on_offer` replaces its transport.
    pub fn reconnect_voice(&mut self) {
        if self.voice_peers.is_empty() {
            return;
        }
        eprintln!("[latency] rebuilding voice transports (jitter buffer now {} ms)", crate::flow_peer::jitter_latency_ms());
        let peers: Vec<Uuid> = self.voice_peers.keys().copied().collect();
        for p in &peers {
            self.stop_voice(*p);
        }
        for p in peers {
            if let Err(e) = self.voice_offer(p) {
                self.emit(SessionEvent::Error(format!("voice reconnect: {e}")));
            }
        }
    }

    /// Start sharing your screen to every current voice member, via the active
    /// `ScreenTransport` (P2P now). Also tells the server so others list you.
    ///
    /// Builds ONE `ScreenSource` (capture + encode + preview); encoded H265 is
    /// fanned from the single appsink into per-viewer appsrcs registered in
    /// `start_offerer`. The `HEARTH_CAPTURE` env var overrides the capture
    /// element entirely, regardless of `cfg`, for bench/dev testing.
    pub fn start_share(&mut self, cfg: ShareConfig) {
        // Going live: drop any preview-only source first so we hold at most one
        // pipeline at a time. The ScreenSource Drop tears down synchronously.
        self.screen_source = None;

        self.share_config = cfg;
        let _ = self.out_tx.send(ClientMessage::ShareStart);

        self.screen_source = screen::ScreenSource::new(&self.share_config, true);
        if self.screen_source.is_none() {
            eprintln!("start_share: ScreenSource unavailable (capture/encode/sink missing) – viewers will receive no video");
        }

        let viewers = self.voice_members.clone();
        let mut t = std::mem::replace(&mut self.screen_transport, Box::new(P2pTransport));
        t.start(self, &viewers);
        self.screen_transport = t;
    }

    /// Stop sharing: unregister all viewers, tear down the source, notify the server.
    ///
    /// `ScreenSource::Drop` sets the pipeline to Null synchronously so the next
    /// `start_share` or `start_preview` does not race against resource release.
    pub fn stop_share(&mut self) {
        let _ = self.out_tx.send(ClientMessage::ShareStop);

        let mut t = std::mem::replace(&mut self.screen_transport, Box::new(P2pTransport));
        t.stop(self);
        self.screen_transport = t;

        // Drop the source after the transport has stopped (and unregistered all
        // viewers), so no in-flight callbacks push into removed appsrcs.
        self.screen_source = None;
    }

    /// Return the local preview paintable for the current share or preview session.
    ///
    /// The returned `glib::Object` is a `gdk::Paintable` and can be cast by
    /// the caller with `obj.dynamic_cast::<gtk4::gdk::Paintable>()`.
    pub fn preview_paintable(&self) -> Option<glib::Object> {
        self.screen_source.as_ref().map(|s| s.paintable())
    }

    /// Start a local-only preview pipeline (no encode, no WebRTC) so the picker
    /// can show a live preview before going live. Any prior source is torn down first.
    pub fn start_preview(&mut self, cfg: ShareConfig) {
        self.stop_preview();
        self.share_config = cfg;
        self.screen_source = screen::ScreenSource::new(&self.share_config, false);
    }

    /// Stop the local preview. No-op when not running.
    pub fn stop_preview(&mut self) {
        self.screen_source = None;
    }

    pub fn start_call(&mut self, peer: Uuid) -> Result<()> {
        self.voice_offer(peer)
    }

    pub(crate) fn start_offerer(&mut self, peer: Uuid, flow: Flow) -> Result<()> {
        let key = (peer, flow);
        if self.peers.contains_key(&key) {
            return Ok(());
        }

        // Screen offerers no longer pass a capture chain: the shared ScreenSource
        // handles capture+encode centrally. The screen_chain and bitrate_kbps
        // params in FlowPeer::new are kept for the legacy run() path and Voice.
        let screen_audio = if flow == Flow::Screen {
            screen::screen_audio_chain(&self.share_config.audio)
        } else {
            None
        };

        let p = FlowPeer::new(
            flow,
            Role::Offerer,
            peer,
            self.sink,
            self.out_tx.clone(),
            self.evt_tx.clone(),
            None,
            screen_audio,
            self.share_config.bitrate_kbps,
        )?;

        if flow == Flow::Screen {
            if let (Some(appsrc), Some(ss)) = (p.screen_appsrc(), self.screen_source.as_ref()) {
                // Register the appsrc with its readiness gate closed: the encoder
                // starts feeding it only once this peer reaches Connected.
                ss.register_viewer(peer, appsrc, p.screen_ready());
            }
        }

        // The offer's video m-line caps come from the send-branch capsfilter, so
        // it is complete even though no encoded frame has flowed yet (the fan-out
        // stays gated until the connection is established).
        p.start_negotiation();

        self.peers.insert(key, p);

        Ok(())
    }

    /// Ensure the shared capture is running and register this Voice flow's send
    /// `appsrc` with it. Starting the mic is best-effort; failure surfaces as an
    /// error event but never tears the flow down.
    fn register_voice_send(&mut self, peer: Uuid) {
        if self.voice_capture.is_none() {
            match VoiceCapture::start(
                self.input_device.clone(),
                self.output_device.clone(),
                self.dsp_config.clone(),
                self.gate.clone(),
                self.evt_tx.clone(),
            ) {
                Ok(vc) => self.voice_capture = Some(vc),
                Err(e) => {
                    self.emit(SessionEvent::Error(format!("voice capture: {e}")));
                    return;
                }
            }
        }

        let appsrc = self.voice_peers.get(&peer).map(|p| p.voice_appsrc());

        if let (Some(vc), Some(appsrc)) = (self.voice_capture.as_ref(), appsrc) {
            vc.add_peer(appsrc);
        }
    }

    /// Unregister a Voice flow's send `appsrc`; drop the whole capture once no
    /// Voice flows remain so the mic and DSP stop.
    fn unregister_voice_send(&mut self, appsrc: Option<gst_app::AppSrc>) {
        if let (Some(vc), Some(appsrc)) = (self.voice_capture.as_ref(), appsrc) {
            vc.remove_peer(&appsrc);
        }

        if self.voice_peers.is_empty() {
            self.voice_capture = None;
        }
    }

    pub fn stop_flow(&mut self, peer: Uuid, flow: Flow) {
        if flow == Flow::Voice {
            self.stop_voice(peer);
            return;
        }
        if let Some(p) = self.peers.remove(&(peer, flow)) {
            let appsrc = p.voice_appsrc();

            if flow == Flow::Screen {
                if let Some(ss) = &self.screen_source {
                    ss.unregister_viewer(&peer);
                }
            }

            p.stop();

            if flow == Flow::Voice {
                self.unregister_voice_send(appsrc);
            }
        }
    }

    /// Tear down every active media flow (the other peers and chat are untouched).
    pub fn stop_all(&mut self) {
        for (_, p) in self.peers.drain() {
            p.stop();
        }

        self.voice_capture = None;
    }

    /// Mute the mic. Gating is now software, so muting flips the shared gate; the
    /// capture callback then pushes silence to every peer.
    pub fn mute(&self, on: bool) {
        self.set_muted(on);
    }

    /// Deafen: silence incoming audio (spk_valve on every voice recv) and mute the mic.
    pub fn deafen(&self, on: bool) {
        for p in self.voice_peers.values() {
            p.set_deaf(on);
        }

        self.set_muted(on);
    }

    /// Apply a new DSP config live (no pipeline rebuild).
    pub fn set_dsp(&mut self, cfg: DspConfig) {
        self.dsp_config = cfg.clone();

        if let Some(vc) = self.voice_capture.as_ref() {
            vc.set_config(cfg);
        }
    }

    /// Change the voice activation mode (voice-activity / push-to-talk / always-on).
    pub fn set_activation(&self, mode: ActivationMode) {
        self.gate.lock().unwrap().set_mode(mode);
    }

    /// Set the jitter-buffer depth (ms). Lower = less latency, more sensitive to
    /// network jitter. `rtpjitterbuffer` only honours `latency` at startup, so a
    /// live change takes effect on the next voice connect — leave/rejoin to test
    /// a new value. (We still poke active peers as a best-effort.)
    pub fn set_jitter_latency_ms(&self, ms: u32) {
        crate::flow_peer::set_jitter_latency_ms(ms);
        for p in self.voice_peers.values() {
            p.set_jitter_ms(ms);
        }
        if !self.voice_peers.is_empty() {
            eprintln!("[latency] jitter buffer -> {ms} ms (effective on next voice connect — leave/rejoin to apply)");
        }
    }

    /// Mute / unmute the mic via the shared gate.
    pub fn set_muted(&self, muted: bool) {
        self.gate.lock().unwrap().set_muted(muted);
    }

    /// Hold / release push-to-talk via the shared gate.
    pub fn set_ptt_held(&self, held: bool) {
        self.gate.lock().unwrap().set_ptt_held(held);
    }

    /// Set (or clear) the global PTT key by name (e.g. `"F12"`, `"space"`).
    ///
    /// Passing `None` removes any existing grab. The grab is only meaningful
    /// while the activation mode is `PushToTalk`; the gate's mode check still
    /// applies on each press/release.
    pub fn set_ptt_key(&mut self, key: Option<String>) {
        // Drop the current grab (releases the X grab and joins the thread).
        self.ptt_grab = None;

        let Some(name) = key else { return };

        let keysym = match keysym_from_name(&name) {
            Some(k) => k,
            None => {
                self.emit(SessionEvent::Error(format!("unknown PTT key: {name}")));
                return;
            }
        };

        let gate = self.gate.clone();

        match PttGrab::grab(keysym, move |held| {
            gate.lock().unwrap().set_ptt_held(held);
        }) {
            Ok(grab) => self.ptt_grab = Some(grab),
            Err(e) => self.emit(SessionEvent::Error(format!("PTT grab failed: {e}"))),
        }
    }

    /// Select the mic input device; restarts the running capture (brief blip).
    pub fn set_input_device(&mut self, dev: Option<String>) {
        self.input_device = dev;
        self.restart_voice_capture();
    }

    /// Select the speaker output device; restarts the capture so AEC references
    /// the new sink's monitor (brief blip).
    pub fn set_output_device(&mut self, dev: Option<String>) {
        self.output_device = dev;
        self.restart_voice_capture();
    }

    /// True when at least one Voice-flow peer is connected. Used to gate the
    /// mic test so it cannot open a second concurrent capture during a call.
    pub fn in_voice(&self) -> bool {
        !self.voice_peers.is_empty()
    }

    /// Start the standalone mic loopback for the Settings mic-test panel.
    ///
    /// Captures the mic, runs DSP, plays it back on the output device, and
    /// emits `SessionEvent::InputLevel`. Refuses silently (with an error
    /// event) when a voice call is active – the engine itself enforces this
    /// to prevent a second concurrent capture + AEC-reference corruption.
    /// Calling while the monitor is already running replaces the previous one.
    pub fn start_mic_test(&mut self) {
        if self.in_voice() {
            self.emit(SessionEvent::Error(
                "mic test unavailable during a call".into(),
            ));
            return;
        }

        self.mic_monitor = None;

        match Monitor::start(
            self.input_device.clone(),
            self.output_device.clone(),
            self.dsp_config.clone(),
            self.evt_tx.clone(),
        ) {
            Ok(m) => self.mic_monitor = Some(m),
            Err(e) => self.emit(SessionEvent::Error(format!("mic test: {e}"))),
        }
    }

    /// Stop the mic-test loopback. No-op if not running.
    pub fn stop_mic_test(&mut self) {
        self.mic_monitor = None;
    }

    /// Rebuild the shared capture with the current devices/config, re-registering
    /// every live Voice flow's send `appsrc`. No-op when no capture is running.
    fn restart_voice_capture(&mut self) {
        if self.voice_capture.is_none() {
            return;
        }

        self.voice_capture = None;

        let appsrcs: Vec<gst_app::AppSrc> =
            self.voice_peers.values().map(|p| p.voice_appsrc()).collect();

        match VoiceCapture::start(
            self.input_device.clone(),
            self.output_device.clone(),
            self.dsp_config.clone(),
            self.gate.clone(),
            self.evt_tx.clone(),
        ) {
            Ok(vc) => {
                for appsrc in appsrcs {
                    vc.add_peer(appsrc);
                }

                self.voice_capture = Some(vc);
            }
            Err(e) => self.emit(SessionEvent::Error(format!("voice capture restart: {e}"))),
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
            ServerMessage::PeerLeft { user } => {
                // Tear down any screen-view flow we hold for this peer. The
                // sharer's process may have crashed without sending ShareStopped,
                // so we treat PeerLeft as an implicit share-stop for the viewer.
                if self.peers.contains_key(&(user, Flow::Screen)) {
                    self.stop_flow(user, Flow::Screen);
                    self.emit(SessionEvent::ShareStopped { user });
                }

                self.emit(SessionEvent::Presence(Presence::Left { user }));
            }
            ServerMessage::Chat { from, username, body, at } => {
                self.emit(SessionEvent::Chat(ChatEntry { from, username, body, at }))
            }
            ServerMessage::ChatHistory { messages } => self.emit(SessionEvent::ChatHistory(messages)),
            ServerMessage::Offer { from, flow, sdp } => {
                // Voice uses the UDP transport: `sdp` carries the peer's ip:port.
                if flow == Flow::Voice {
                    self.voice_on_offer(from, &sdp);
                    return;
                }

                let key = (from, flow);

                // A fresh offer starts a new session for this flow. Drop any stale
                // peer first so re-sharing after a Stop renegotiates cleanly (and
                // releases the old webrtcbin's ICE port).
                if let Some(old) = self.peers.remove(&key) {
                    old.stop();
                }

                match FlowPeer::new(flow, Role::Answerer, from, self.sink, self.out_tx.clone(), self.evt_tx.clone(), None, None, 0) {
                    Ok(p) => {
                        p.handle_offer(&sdp);
                        self.peers.insert(key, p);
                    }
                    Err(e) => self.emit(SessionEvent::Error(format!("create answerer: {e}"))),
                }
            }
            ServerMessage::Answer { from, flow, sdp } => {
                if flow == Flow::Voice {
                    self.voice_on_answer(from, &sdp);
                    return;
                }
                if let Some(p) = self.peers.get(&(from, flow)) {
                    p.handle_answer(&sdp);
                }
            }
            ServerMessage::Ice { from, flow, mline, candidate } => {
                // No ICE for the UDP voice transport.
                if flow == Flow::Voice {
                    return;
                }
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
                self_name: String::new(),
                voice_members: Vec::new(),
                screen_transport: Box::new(P2pTransport),
                share_config: ShareConfig::default(),
                peers: HashMap::new(),
                voice_peers: HashMap::new(),
                voice_capture: None,
                mic_monitor: None,
                gate: Arc::new(Mutex::new(Gate::new(ActivationMode::Voice { threshold: -45.0 }))),
                dsp_config: default_dsp_config(),
                input_device: None,
                output_device: None,
                ptt_grab: None,
                _client: None,
                screen_source: None,
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
