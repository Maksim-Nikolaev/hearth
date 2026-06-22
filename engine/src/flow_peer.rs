use crate::{
    capture, encoders,
    flow::VideoSink,
    signaling::{login, SignalingClient},
};
use anyhow::Result;
use gstreamer as gst;
use gstreamer::glib;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
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
                let chain = capture::capture_chain();
                build_screen_send_branch(&pipeline, &webrtc, encoder, &chain, None)?;
            }
            Flow::Voice => {
                build_voice_send_branch(&pipeline, &webrtc)?;
            }
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

    // Incoming media (answerer): demux by caps so screen flows handle both the
    // video track and the optional audio track (payload 98).
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
            Flow::Screen => {
                // Distinguish the audio track from the video track by caps.
                let is_audio = pad
                    .current_caps()
                    .as_ref()
                    .and_then(|c| c.structure(0))
                    .map(|s| s.get::<&str>("media").map(|m| m == "audio").unwrap_or(false))
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

/// Build the screenshare send branch and link it to `webrtc`.
///
/// `chain` is the full GStreamer source + caps string. Pass the result of
/// `screen::capture_chain(&cfg)` for the product path, or
/// `capture::capture_chain()` for the legacy/standalone path. The chain must
/// already embed a `video/x-raw` capsfilter so the encoder receives a known
/// format.
///
/// `audio_chain` is an optional string for the audio source chain produced by
/// `screen::screen_audio_chain`. When `Some`, a second audio media is added to
/// the same `webrtcbin` at payload type 98 (distinct from voice at 97).
pub(crate) fn build_screen_send_branch(
    pipeline: &gst::Pipeline,
    webrtc: &gst::Element,
    encoder: &str,
    chain: &str,
    audio_chain: Option<&str>,
) -> Result<()> {
    let cap = gst::parse::bin_from_description(chain, true)?;

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

    pipeline.add_many([cap.upcast_ref(), &enc, &parse, &pay, &caps])?;
    gst::Element::link_many([cap.upcast_ref(), &enc, &parse, &pay, &caps])?;
    caps.link(webrtc)?;

    if let Some(achain) = audio_chain {
        build_screen_audio_send_branch(pipeline, webrtc, achain)?;
    }

    Ok(())
}

/// Add a second audio media track to an existing screen `webrtcbin`.
///
/// Uses payload type 98 to avoid clashing with voice (97). No DSP is applied:
/// the captured audio is encoded directly to Opus and sent raw.
fn build_screen_audio_send_branch(
    pipeline: &gst::Pipeline,
    webrtc: &gst::Element,
    audio_chain: &str,
) -> Result<()> {
    let src_bin = gst::parse::bin_from_description(audio_chain, true)?;

    let pay = gst::ElementFactory::make("rtpopuspay").build()?;
    let caps = gst::ElementFactory::make("capsfilter")
        .property(
            "caps",
            gst::Caps::builder("application/x-rtp")
                .field("media", "audio")
                .field("encoding-name", "OPUS")
                .field("payload", 98i32)
                .build(),
        )
        .build()?;

    pipeline.add_many([src_bin.upcast_ref(), &pay, &caps])?;
    gst::Element::link_many([src_bin.upcast_ref(), &pay, &caps])?;
    caps.link(webrtc)?;

    Ok(())
}

/// Link an incoming screenshare audio pad (OPUS, payload 98) to an audio sink.
///
/// Used by the screen answerer to play the sharer's captured system/app audio.
/// Distinct from the voice recv chain: no DSP, no valve, direct to `autoaudiosink`.
pub(crate) fn link_screen_audio_recv(pipeline: &gst::Pipeline, pad: &gst::Pad) {
    let depay = gst::ElementFactory::make("rtpopusdepay").build().unwrap();
    let dec = gst::ElementFactory::make("opusdec").build().unwrap();
    let conv = gst::ElementFactory::make("audioconvert").build().unwrap();
    let resample = gst::ElementFactory::make("audioresample").build().unwrap();
    let sink = gst::ElementFactory::make("autoaudiosink").property("sync", false).build().unwrap();

    pipeline.add_many([&depay, &dec, &conv, &resample, &sink]).unwrap();
    gst::Element::link_many([&depay, &dec, &conv, &resample, &sink]).unwrap();

    for e in [&depay, &dec, &conv, &resample, &sink] {
        e.sync_state_with_parent().unwrap();
    }

    let sink_pad = depay.static_pad("sink").unwrap();

    if let Err(e) = pad.link(&sink_pad) {
        eprintln!("screen audio pad link failed: {e}");
        return;
    }

    eprintln!("incoming screen audio linked -> playing");
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

/// The voice send branch is fed by an `appsrc` that [`VoiceCapture`] pushes the
/// single DSP'd mic frame into. Software gating happens in the capture callback,
/// so there is no `mic_valve` here. Returns the `voice_in` appsrc for the session
/// to register with the capture.
pub(crate) fn build_voice_send_branch(
    pipeline: &gst::Pipeline,
    webrtc: &gst::Element,
) -> Result<gst_app::AppSrc> {
    let pcm_caps = gst::Caps::builder("audio/x-raw")
        .field("format", "S16LE")
        .field("channels", 1i32)
        .field("rate", 48000i32)
        .field("layout", "interleaved")
        .build();

    let appsrc = gst_app::AppSrc::builder()
        .name("voice_in")
        .caps(&pcm_caps)
        .is_live(true)
        .format(gst::Format::Time)
        .build();

    let convert = gst::ElementFactory::make("audioconvert").build()?;
    let resample = gst::ElementFactory::make("audioresample").build()?;
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

    pipeline.add_many([appsrc.upcast_ref(), &convert, &resample, &enc, &pay, &caps])?;
    gst::Element::link_many([appsrc.upcast_ref(), &convert, &resample, &enc, &pay, &caps])?;
    caps.link(webrtc)?;

    Ok(appsrc)
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
        ServerMessage::VoiceState { .. }
        | ServerMessage::VoiceJoined { .. }
        | ServerMessage::VoiceLeft { .. }
        | ServerMessage::ShareStarted { .. }
        | ServerMessage::ShareStopped { .. } => {}
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
