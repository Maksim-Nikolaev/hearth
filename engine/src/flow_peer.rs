use crate::{
    capture, encoders,
    flow::VideoSink,
    signaling::{login, SignalingClient},
};
use anyhow::Result;
use gstreamer as gst;
use gstreamer::glib;
use gstreamer::prelude::*;
use gstreamer_sdp as gst_sdp;
use gstreamer_webrtc as gst_webrtc;
use hearth_protocol::{ClientMessage, Flow, ServerMessage};
use std::sync::{Arc, Mutex};
use uuid::Uuid;

/// Invoked once, on the calling thread, with the incoming video's `gdk::Paintable`
/// (typed opaquely as a `glib::Object` so the engine stays decoupled from gtk4).
pub type PaintableCb = Box<dyn FnOnce(glib::Object)>;

/// One media flow between this peer and one other, carried by a single
/// `webrtcbin`. `share == true` captures and offers; otherwise it answers and
/// displays/plays.
pub struct PeerConfig<'a> {
    pub http_base: &'a str,
    pub ws_base: &'a str,
    pub username: &'a str,
    pub password: &'a str,
    pub room: &'a str,
    pub share: bool,
    pub flow: Flow,
    pub sink: VideoSink,
}

struct State {
    target: Mutex<Option<Uuid>>,
    pending_ice: Mutex<Vec<(u32, String)>>,
    offer_created: Mutex<bool>,
    flow: Flow,
}

pub async fn run(cfg: PeerConfig<'_>, mut on_paintable: Option<PaintableCb>) -> Result<()> {
    gst::init()?;

    let token = login(cfg.http_base, cfg.username, cfg.password).await?;
    let (client, mut inbound) = SignalingClient::connect(cfg.ws_base, &token).await?;
    let client = Arc::new(client);
    let state = Arc::new(State {
        target: Mutex::new(None),
        pending_ice: Mutex::new(Vec::new()),
        offer_created: Mutex::new(false),
        flow: cfg.flow,
    });

    let pipeline = gst::Pipeline::new();
    let webrtc = gst::ElementFactory::make("webrtcbin")
        .name("wrtc")
        .property_from_str("stun-server", "stun://stun.l.google.com:19302")
        .build()?;

    // Optional TURN relay, e.g. HEARTH_TURN="turn://user:pass@host:3478".
    if let Ok(turn) = std::env::var("HEARTH_TURN") {
        if !turn.trim().is_empty() {
            webrtc.set_property_from_str("turn-server", &turn);
            println!("using TURN relay: {turn}");
        }
    }

    pipeline.add(&webrtc)?;

    // Surface pipeline errors/warnings instead of letting element failures stay silent.
    let bus = pipeline.bus().expect("pipeline has a bus");
    let _bus_watch = bus.add_watch(move |_, msg| {
        use gst::MessageView;

        match msg.view() {
            MessageView::Error(e) => {
                eprintln!(
                    "pipeline error from {:?}: {} ({:?})",
                    e.src().map(|s| s.path_string()),
                    e.error(),
                    e.debug()
                );
            }
            MessageView::Warning(w) => {
                eprintln!("pipeline warning: {} ({:?})", w.error(), w.debug());
            }
            _ => {}
        }

        glib::ControlFlow::Continue
    })?;

    if cfg.share {
        match cfg.flow {
            Flow::Screen => {
                let encoder = encoders::detect().0.unwrap_or("x265enc");
                build_screen_send_branch(&pipeline, &webrtc, encoder)?;
            }
            Flow::Voice => build_voice_send_branch(&pipeline, &webrtc)?,
            Flow::Webcam => anyhow::bail!("webcam flow is out of M5 scope"),
        }
    }

    // Pre-create the display sink (video flows only) so a gtk4paintablesink's
    // paintable is read on the caller's (main) thread, never on a streaming thread.
    let video_sink = if cfg.flow == Flow::Voice {
        None
    } else {
        let s = match cfg.sink {
            VideoSink::Auto => gst::ElementFactory::make("autovideosink")
                .property("sync", false)
                .build()?,
            VideoSink::Paintable => {
                let s = gst::ElementFactory::make("gtk4paintablesink").build()?;
                if let Some(cb) = on_paintable.take() {
                    cb(s.property::<glib::Object>("paintable"));
                }
                s
            }
        };
        Some(Arc::new(s))
    };

    // Incoming media (answerer): the flow fixes the media type, so no caps-sniffing.
    let pipeline_weak = pipeline.downgrade();
    let flow = cfg.flow;
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

    // Local ICE -> signaling (buffer until we know the target).
    {
        let client = client.clone();
        let state = state.clone();
        webrtc.connect("on-ice-candidate", false, move |vals| {
            let mline = vals[1].get::<u32>().unwrap();
            let cand = vals[2].get::<String>().unwrap();

            let target = *state.target.lock().unwrap();
            match target {
                Some(to) => client.send(ClientMessage::Ice { to, flow: state.flow, mline, candidate: cand }),
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
    client.send(ClientMessage::Join { room: cfg.room.to_string() });

    // Drive negotiation from inbound signaling.
    let webrtc_loop = webrtc.clone();
    let client_loop = client.clone();
    let state_loop = state.clone();
    let share = cfg.share;
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

pub(crate) fn build_screen_send_branch(pipeline: &gst::Pipeline, webrtc: &gst::Element, encoder: &str) -> Result<()> {
    let cap = gst::parse::bin_from_description(&capture::capture_chain(), true)?;

    let rate = gst::ElementFactory::make("videorate").build()?;
    let scale = gst::ElementFactory::make("videoscale").build()?;
    let raw_caps = gst::ElementFactory::make("capsfilter")
        .property("caps", capture::video_caps().parse::<gst::Caps>()?)
        .build()?;

    let enc = gst::ElementFactory::make(encoder).build()?;
    tune_encoder(&enc);

    let parse = gst::ElementFactory::make("h265parse").build()?;
    let pay = gst::ElementFactory::make("rtph265pay")
        .property("config-interval", -1i32)
        .build()?;
    let caps = gst::ElementFactory::make("capsfilter")
        .property(
            "caps",
            gst::Caps::builder("application/x-rtp")
                .field("media", "video")
                .field("encoding-name", "H265")
                .field("payload", 96i32)
                .build(),
        )
        .build()?;

    pipeline.add_many([cap.upcast_ref(), &rate, &scale, &raw_caps, &enc, &parse, &pay, &caps])?;
    gst::Element::link_many([cap.upcast_ref(), &rate, &scale, &raw_caps, &enc, &parse, &pay, &caps])?;
    caps.link(webrtc)?;

    Ok(())
}

pub(crate) fn link_video_recv(pipeline: &gst::Pipeline, pad: &gst::Pad, vsink: &gst::Element) {
    let depay = gst::ElementFactory::make("rtph265depay").build().unwrap();
    let parse = gst::ElementFactory::make("h265parse").build().unwrap();
    let dec = gst::ElementFactory::make("avdec_h265").build().unwrap();
    let conv = gst::ElementFactory::make("videoconvert").build().unwrap();

    pipeline.add_many([&depay, &parse, &dec, &conv]).unwrap();
    pipeline.add(vsink).unwrap();
    gst::Element::link_many([&depay, &parse, &dec, &conv, vsink]).unwrap();
    for e in [&depay, &parse, &dec, &conv] {
        e.sync_state_with_parent().unwrap();
    }
    vsink.sync_state_with_parent().unwrap();
    pad.link(&depay.static_pad("sink").unwrap()).unwrap();
    println!("incoming video linked -> displaying");
}

pub(crate) fn link_voice_recv(pipeline: &gst::Pipeline, pad: &gst::Pad) {
    let depay = gst::ElementFactory::make("rtpopusdepay").build().unwrap();
    let dec = gst::ElementFactory::make("opusdec").build().unwrap();
    let conv = gst::ElementFactory::make("audioconvert").build().unwrap();
    let resample = gst::ElementFactory::make("audioresample").build().unwrap();
    // deafen gate: drop=true silences incoming audio without tearing the flow down.
    let valve = gst::ElementFactory::make("valve").name("spk_valve").property("drop", false).build().unwrap();
    let sink = gst::ElementFactory::make("autoaudiosink").property("sync", false).build().unwrap();

    pipeline.add_many([&depay, &dec, &conv, &resample, &valve, &sink]).unwrap();
    gst::Element::link_many([&depay, &dec, &conv, &resample, &valve, &sink]).unwrap();
    for e in [&depay, &dec, &conv, &resample, &valve, &sink] {
        e.sync_state_with_parent().unwrap();
    }
    pad.link(&depay.static_pad("sink").unwrap()).unwrap();
    println!("incoming voice linked -> playing");
}

pub(crate) fn build_voice_send_branch(pipeline: &gst::Pipeline, webrtc: &gst::Element) -> Result<()> {
    let src = gst::parse::bin_from_description(
        "autoaudiosrc ! audioconvert ! audioresample ! queue",
        true,
    )?;
    // mute gate: drop=true stops sending mic audio without renegotiating.
    let valve = gst::ElementFactory::make("valve").name("mic_valve").property("drop", false).build()?;
    let enc = gst::ElementFactory::make("opusenc").build()?;
    let pay = gst::ElementFactory::make("rtpopuspay").build()?;
    let caps = gst::ElementFactory::make("capsfilter")
        .property(
            "caps",
            gst::Caps::builder("application/x-rtp")
                .field("media", "audio")
                .field("encoding-name", "OPUS")
                .field("payload", 97i32)
                .build(),
        )
        .build()?;

    pipeline.add_many([src.upcast_ref(), &valve, &enc, &pay, &caps])?;
    gst::Element::link_many([src.upcast_ref(), &valve, &enc, &pay, &caps])?;
    caps.link(webrtc)?;

    Ok(())
}

fn tune_encoder(enc: &gst::Element) {
    let bitrate = std::env::var("HEARTH_BITRATE_KBPS").unwrap_or_else(|_| "8000".into());

    set_if_present(enc, "bitrate", &bitrate);
    set_if_present(enc, "key-int-max", "60");
}

fn set_if_present(el: &gst::Element, prop: &str, val: &str) {
    if el.find_property(prop).is_some() {
        el.set_property_from_str(prop, val);
        println!("encoder: set {prop}={val}");
    }
}

fn handle_signal(
    webrtc: &gst::Element,
    client: &SignalingClient,
    state: &Arc<State>,
    share: bool,
    msg: ServerMessage,
) {
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
        ServerMessage::Offer { from, flow: _, sdp } => {
            *state.target.lock().unwrap() = Some(from);
            flush_ice(client, state);

            let sdp = gst_sdp::SDPMessage::parse_buffer(sdp.as_bytes()).unwrap();
            let offer = gst_webrtc::WebRTCSessionDescription::new(gst_webrtc::WebRTCSDPType::Offer, sdp);
            webrtc.emit_by_name::<()>("set-remote-description", &[&offer, &None::<gst::Promise>]);

            let w = webrtc.clone();
            let to = from;
            let flow = state.flow;
            let tx = client.sender();
            let promise = gst::Promise::with_change_func(move |reply| {
                let Ok(Some(reply)) = reply else {
                    return;
                };
                let answer = reply
                    .value("answer")
                    .unwrap()
                    .get::<gst_webrtc::WebRTCSessionDescription>()
                    .unwrap();
                w.emit_by_name::<()>("set-local-description", &[&answer, &None::<gst::Promise>]);
                let _ = tx.send(ClientMessage::Answer {
                    to,
                    flow,
                    sdp: answer.sdp().as_text().unwrap().to_string(),
                });
            });
            webrtc.emit_by_name::<()>("create-answer", &[&None::<gst::Structure>, &promise]);
        }
        ServerMessage::Answer { from: _, flow: _, sdp } => {
            let sdp = gst_sdp::SDPMessage::parse_buffer(sdp.as_bytes()).unwrap();
            let answer = gst_webrtc::WebRTCSessionDescription::new(gst_webrtc::WebRTCSDPType::Answer, sdp);
            webrtc.emit_by_name::<()>("set-remote-description", &[&answer, &None::<gst::Promise>]);
        }
        ServerMessage::Ice { from: _, flow: _, mline, candidate } => {
            webrtc.emit_by_name::<()>("add-ice-candidate", &[&mline, &candidate]);
        }
        ServerMessage::PeerLeft { .. } => {}
        ServerMessage::Chat { .. } | ServerMessage::ChatHistory { .. } => {}
    }
}

fn set_target_and_offer(webrtc: &gst::Element, client: &SignalingClient, state: &Arc<State>, target: Uuid) {
    {
        let mut t = state.target.lock().unwrap();
        if t.is_some() {
            return;
        }
        *t = Some(target);
    }
    flush_ice(client, state);

    let mut created = state.offer_created.lock().unwrap();
    if *created {
        return;
    }
    *created = true;

    let w = webrtc.clone();
    let flow = state.flow;
    let tx = client.sender();
    let promise = gst::Promise::with_change_func(move |reply| {
        let Ok(Some(reply)) = reply else {
            return;
        };
        let offer = reply
            .value("offer")
            .unwrap()
            .get::<gst_webrtc::WebRTCSessionDescription>()
            .unwrap();
        w.emit_by_name::<()>("set-local-description", &[&offer, &None::<gst::Promise>]);
        let _ = tx.send(ClientMessage::Offer {
            to: target,
            flow,
            sdp: offer.sdp().as_text().unwrap().to_string(),
        });
    });
    webrtc.emit_by_name::<()>("create-offer", &[&None::<gst::Structure>, &promise]);
}

fn flush_ice(client: &SignalingClient, state: &Arc<State>) {
    let target = match *state.target.lock().unwrap() {
        Some(t) => t,
        None => return,
    };
    let mut pending = state.pending_ice.lock().unwrap();

    for (mline, candidate) in pending.drain(..) {
        client.send(ClientMessage::Ice { to: target, flow: state.flow, mline, candidate });
    }
}
