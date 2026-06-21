use crate::{
    capture, encoders,
    signaling::{login, SignalingClient},
};
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

pub async fn run(
    http_base: &str,
    ws_base: &str,
    username: &str,
    password: &str,
    room: &str,
    share: bool,
) -> Result<()> {
    gst::init()?;

    let token = login(http_base, username, password).await?;
    let (client, mut inbound) = SignalingClient::connect(ws_base, &token).await?;
    let client = Arc::new(client);
    let state = Arc::new(State {
        target: Mutex::new(None),
        pending_ice: Mutex::new(Vec::new()),
        offer_created: Mutex::new(false),
    });

    let pipeline = gst::Pipeline::new();
    let webrtc = gst::ElementFactory::make("webrtcbin")
        .name("wrtc")
        .property_from_str("stun-server", "stun://stun.l.google.com:19302")
        .build()?;
    pipeline.add(&webrtc)?;

    if share {
        let encoder = encoders::detect().0.unwrap_or("x265enc");

        build_send_branch(&pipeline, &webrtc, encoder)?;
    }

    // Incoming media (viewer): decode + display.
    let pipeline_weak = pipeline.downgrade();
    webrtc.connect_pad_added(move |_w, pad| {
        if pad.direction() != gst::PadDirection::Src {
            return;
        }
        let Some(pipeline) = pipeline_weak.upgrade() else {
            return;
        };

        let depay = gst::ElementFactory::make("rtph265depay").build().unwrap();
        let parse = gst::ElementFactory::make("h265parse").build().unwrap();
        let dec = gst::ElementFactory::make("avdec_h265").build().unwrap();
        let conv = gst::ElementFactory::make("videoconvert").build().unwrap();
        let sink = gst::ElementFactory::make("autovideosink")
            .property("sync", false)
            .build()
            .unwrap();

        pipeline.add_many([&depay, &parse, &dec, &conv, &sink]).unwrap();
        gst::Element::link_many([&depay, &parse, &dec, &conv, &sink]).unwrap();
        for e in [&depay, &parse, &dec, &conv, &sink] {
            e.sync_state_with_parent().unwrap();
        }
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

    pipeline.add_many([cap.upcast_ref(), &enc, &parse, &pay, &caps])?;
    gst::Element::link_many([cap.upcast_ref(), &enc, &parse, &pay, &caps])?;
    caps.link(webrtc)?;

    Ok(())
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
        ServerMessage::Offer { from, sdp } => {
            *state.target.lock().unwrap() = Some(from);
            flush_ice(client, state);

            let sdp = gst_sdp::SDPMessage::parse_buffer(sdp.as_bytes()).unwrap();
            let offer =
                gst_webrtc::WebRTCSessionDescription::new(gst_webrtc::WebRTCSDPType::Offer, sdp);
            webrtc.emit_by_name::<()>("set-remote-description", &[&offer, &None::<gst::Promise>]);

            let w = webrtc.clone();
            let to = from;
            let tx = client.sender();
            // Create the answer; send it back to `from`.
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
                    sdp: answer.sdp().as_text().unwrap().to_string(),
                });
            });
            webrtc.emit_by_name::<()>("create-answer", &[&None::<gst::Structure>, &promise]);
        }
        ServerMessage::Answer { from: _, sdp } => {
            let sdp = gst_sdp::SDPMessage::parse_buffer(sdp.as_bytes()).unwrap();
            let answer =
                gst_webrtc::WebRTCSessionDescription::new(gst_webrtc::WebRTCSDPType::Answer, sdp);
            webrtc.emit_by_name::<()>("set-remote-description", &[&answer, &None::<gst::Promise>]);
        }
        ServerMessage::Ice { from: _, mline, candidate } => {
            webrtc.emit_by_name::<()>("add-ice-candidate", &[&mline, &candidate]);
        }
        ServerMessage::PeerLeft { .. } => {}
    }
}

fn set_target_and_offer(
    webrtc: &gst::Element,
    client: &SignalingClient,
    state: &Arc<State>,
    target: Uuid,
) {
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
        client.send(ClientMessage::Ice { to: target, mline, candidate });
    }
}
