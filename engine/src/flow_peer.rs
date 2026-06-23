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

/// Process-wide jitter-buffer depth in ms applied to each new `webrtcbin`.
/// `u32::MAX` is the "not yet initialised" sentinel so an explicit 0 is allowed.
static JITTER_MS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(u32::MAX);

/// Jitter-buffer depth in ms for a newly created `webrtcbin`. Defaults to
/// `HEARTH_JITTER_MS` (or 40) until overridden by [`set_jitter_latency_ms`].
/// Changing it affects connections established afterwards, not live ones.
pub(crate) fn jitter_latency_ms() -> u32 {
    use std::sync::atomic::Ordering;
    match JITTER_MS.load(Ordering::Relaxed) {
        u32::MAX => {
            let init = std::env::var("HEARTH_JITTER_MS")
                .ok()
                .and_then(|v| v.trim().parse().ok())
                .unwrap_or(40);
            JITTER_MS.store(init, Ordering::Relaxed);
            init
        },
        v => v,
    }
}

/// Set the jitter-buffer depth used by subsequently created peer connections.
pub fn set_jitter_latency_ms(ms: u32) {
    JITTER_MS.store(ms, std::sync::atomic::Ordering::Relaxed);
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
        // Jitter-buffer depth. webrtcbin defaults to 200 ms, which dominates the
        // end-to-end audio/video delay; 40 ms is a much better fit for a P2P
        // close-friends mesh on a decent network. Raise HEARTH_JITTER_MS on a
        // lossy/high-jitter path if audio starts dropping.
        .property("latency", jitter_latency_ms())
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
                build_screen_send_branch(&pipeline, &webrtc, encoder, &chain, None, 6000)?;
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
                // sync=false: render on arrival for low-latency live video and to
                // avoid dropping frames judged "too late" (see session.rs).
                let s = gst::ElementFactory::make("gtk4paintablesink")
                    .property("sync", false)
                    .build()?;
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
    bitrate_kbps: u32,
) -> Result<()> {
    let cap = gst::parse::bin_from_description(chain, true)?;

    let enc = gst::ElementFactory::make(encoder).build()?;
    tune_encoder(&enc, bitrate_kbps);

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

    // The audio branch is best-effort: a missing PipeWire node, or a source that
    // stopped producing between listing and Go Live, must NOT take down the whole
    // share. Degrade to video-only and record why.
    if let Some(achain) = audio_chain {
        eprintln!("screen audio branch: {achain}");
        match build_screen_audio_send_branch(pipeline, webrtc, achain) {
            Ok(()) => eprintln!("screen audio branch linked"),
            Err(e) => eprintln!("screen audio branch failed, continuing video-only: {e}"),
        }
    }

    Ok(())
}

/// Re-base a buffer's PTS/DTS onto a 0-origin set by the first buffer seen on a
/// per-viewer `appsrc` (`base` holds that first PTS in nanoseconds; `u64::MAX`
/// means unset).
///
/// The shared `ScreenSource` stamps frames against its own pipeline clock, whose
/// running-time is far ahead of a freshly-started viewer pipeline. Forwarded
/// unchanged, webrtcbin's transport sink clock-syncs each RTP buffer to that
/// far-future time and waits ~forever to send it, so video never reaches the
/// peer (the connection is up but the stage stays black). A 0-origin keeps
/// timestamps at/behind the viewer pipeline's running-time, so RTP goes out now.
fn rebase_to_origin(buffer: &mut gst::Buffer, base: &std::sync::atomic::AtomicU64) {
    use std::sync::atomic::Ordering;

    let Some(pts) = buffer.pts() else { return };

    let origin = match base.compare_exchange(u64::MAX, pts.nseconds(), Ordering::Relaxed, Ordering::Relaxed) {
        Ok(_) => pts.nseconds(),
        Err(existing) => existing,
    };

    let buf = buffer.make_mut();
    buf.set_pts(gst::ClockTime::from_nseconds(pts.nseconds().saturating_sub(origin)));
    let dts = buf.dts();
    buf.set_dts(dts.map(|d| gst::ClockTime::from_nseconds(d.nseconds().saturating_sub(origin))));
}

/// Screen offerer send branch fed by the shared `ScreenSource` appsink (no
/// per-peer capture/encode). The caller registers the returned `AppSrc` with
/// `ScreenSource::register_viewer` so encoded H265 frames flow into it.
///
/// Branch: appsrc → rtph265pay → capsfilter → webrtcbin. The optional audio
/// track is added best-effort via `build_screen_audio_send_branch`, identical
/// to the legacy `build_screen_send_branch` behaviour.
pub(crate) fn build_screen_send_appsrc_branch(
    pipeline: &gst::Pipeline,
    webrtc: &gst::Element,
    audio_chain: Option<&str>,
) -> Result<gst_app::AppSrc> {
    let h265_caps = gst::Caps::builder("video/x-h265")
        .field("stream-format", "byte-stream")
        .field("alignment", "au")
        .build();

    // Bounded + `leaky-type=downstream` so a slow viewer drops the oldest queued
    // frames instead of growing memory unbounded or blocking the shared fan-out
    // (the appsink callback pushes here while holding the viewer-registry lock,
    // so `block` must stay false).
    let appsrc = gst_app::AppSrc::builder()
        .name("screen_in")
        .caps(&h265_caps)
        .is_live(true)
        .format(gst::Format::Time)
        .build();
    appsrc.set_property("block", false);
    appsrc.set_property("max-bytes", 2_000_000u64);
    appsrc.set_property_from_str("leaky-type", "downstream");

    // Re-base each access unit onto a per-viewer 0-origin so RTP is sent
    // immediately instead of clock-synced to the shared encoder's far-future
    // timestamps. Own base per appsrc, so a peer joining an ongoing share also
    // starts at 0. See [`rebase_to_origin`].
    if let Some(src_pad) = appsrc.static_pad("src") {
        let base = Arc::new(std::sync::atomic::AtomicU64::new(u64::MAX));
        src_pad.add_probe(gst::PadProbeType::BUFFER, move |_, info| {
            if let Some(gst::PadProbeData::Buffer(buffer)) = &mut info.data {
                rebase_to_origin(buffer, &base);
            }
            gst::PadProbeReturn::Ok
        });
    }

    // The appsrc carries raw byte-stream H265 with no codec_data, so an
    // `h265parse` must rebuild the parameter sets / caps before payloading.
    let parse = gst::ElementFactory::make("h265parse")
        .property("config-interval", -1i32)
        .build()?;

    // Force byte-stream into the payloader. A viewer fanned an ongoing share
    // starts mid-GOP, so `h265parse` has not yet seen VPS/SPS/PPS and cannot
    // build `hvc1` codec_data; left to negotiate it advertises `hvc1` anyway
    // (its preferred format) but emits length-prefixed NALs with no codec_data,
    // which `rtph265pay` then misparses as a byte-stream ("NAL of size 0") and
    // produces zero RTP. Byte-stream needs no codec_data – the parameter sets
    // arrive in-band before each IDR (config-interval=-1) – so the payloader
    // works regardless of where in the GOP the viewer joins.
    let bytestream = gst::ElementFactory::make("capsfilter")
        .property(
            "caps",
            gst::Caps::builder("video/x-h265")
                .field("stream-format", "byte-stream")
                .field("alignment", "au")
                .build(),
        )
        .build()?;

    let pay = gst::ElementFactory::make("rtph265pay")
        .property("config-interval", -1i32)
        .build()?;
    let caps = gst::ElementFactory::make("capsfilter")
        .property(
            "caps",
            gst::Caps::builder("application/x-rtp")
                .field("media", "video")
                .field("encoding-name", "H265")
                .field("clock-rate", 90000i32)
                .field("payload", 96i32)
                .build(),
        )
        .build()?;

    pipeline.add_many([appsrc.upcast_ref(), &parse, &bytestream, &pay, &caps])?;
    gst::Element::link_many([appsrc.upcast_ref(), &parse, &bytestream, &pay, &caps])?;
    caps.link(webrtc)?;

    if let Some(achain) = audio_chain {
        eprintln!("screen audio branch: {achain}");
        match build_screen_audio_send_branch(pipeline, webrtc, achain) {
            Ok(()) => eprintln!("screen audio branch linked"),
            Err(e) => eprintln!("screen audio branch failed, continuing video-only: {e}"),
        }
    }

    Ok(appsrc)
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
    // Runs on a streaming thread: a failure here must log and bail, never panic
    // the whole process (a panic on this thread aborts the app).
    if let Err(e) = try_link_screen_audio_recv(pipeline, pad) {
        eprintln!("screen audio recv link failed, dropping audio track: {e}");
        return;
    }

    eprintln!("incoming screen audio linked -> playing");
}

fn try_link_screen_audio_recv(pipeline: &gst::Pipeline, pad: &gst::Pad) -> Result<()> {
    let depay = gst::ElementFactory::make("rtpopusdepay").build()?;
    let dec = gst::ElementFactory::make("opusdec").build()?;
    let conv = gst::ElementFactory::make("audioconvert").build()?;
    let resample = gst::ElementFactory::make("audioresample").build()?;
    let sink = audio_recv_sink();

    pipeline.add_many([&depay, &dec, &conv, &resample, &sink])?;
    gst::Element::link_many([&depay, &dec, &conv, &resample, &sink])?;

    for e in [&depay, &dec, &conv, &resample, &sink] {
        e.sync_state_with_parent()?;
    }

    let sink_pad = depay
        .static_pad("sink")
        .ok_or_else(|| anyhow::anyhow!("rtpopusdepay has no sink pad"))?;

    pad.link(&sink_pad)?;

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

/// Playback sink for received audio. On Windows use `wasapi2sink low-latency`
/// (autoaudiosink doesn't expose buffer tuning, and WASAPI's default ring buffer
/// adds up to ~1 s of delay); elsewhere `autoaudiosink`. `sync=false` renders on
/// arrival — the webrtcbin jitter buffer already handles de-jitter.
fn audio_recv_sink() -> gst::Element {
    #[cfg(target_os = "windows")]
    {
        // sync=true (clock-synced) is deliberate: with sync=false the WASAPI
        // ring buffer accumulates and audio latency grows unbounded (~1 s+ over
        // a call). Synced playback stays bounded by the jitter buffer; low-latency
        // keeps the device buffer small.
        gst::ElementFactory::make("wasapi2sink")
            .property("low-latency", true)
            .property("sync", true)
            .build()
            .expect("create wasapi2sink")
    }
    #[cfg(not(target_os = "windows"))]
    {
        gst::ElementFactory::make("autoaudiosink")
            .property("sync", false)
            .build()
            .expect("create autoaudiosink")
    }
}

pub(crate) fn link_voice_recv(pipeline: &gst::Pipeline, pad: &gst::Pad) {
    let depay = gst::ElementFactory::make("rtpopusdepay").build().unwrap();
    let dec = gst::ElementFactory::make("opusdec").build().unwrap();
    let conv = gst::ElementFactory::make("audioconvert").build().unwrap();
    let resample = gst::ElementFactory::make("audioresample").build().unwrap();
    // deafen gate: drop=true silences incoming audio without tearing the flow down.
    let valve = gst::ElementFactory::make("valve").name("spk_valve").property("drop", false).build().unwrap();
    let sink = audio_recv_sink();

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

/// Apply bitrate and low-latency settings to the screenshare encoder.
///
/// `bitrate_kbps` is the configured target; `HEARTH_BITRATE_KBPS` env var
/// overrides it when set. All properties are applied via `set_if_present` so
/// missing props on any encoder (x265enc, vah265lpenc, etc.) are silently skipped.
pub(crate) fn tune_encoder(enc: &gst::Element, bitrate_kbps: u32) {
    let bitrate = std::env::var("HEARTH_BITRATE_KBPS")
        .unwrap_or_else(|_| bitrate_kbps.to_string());

    set_if_present(enc, "bitrate", &bitrate);

    // Short GOP keeps seeks and recovery fast; 60 frames at 30 fps = 2 s.
    set_if_present(enc, "key-int-max", "60");

    // CBR gives predictable throughput and avoids bitrate spikes that cause
    // receiver buffer bloat and added latency.
    set_if_present(enc, "rate-control", "cbr");

    // target-usage 7 = maximum encode speed; trades quality for lower latency
    // on VA-API (vah265enc) encoders (range 1 = best quality, 7 = fastest).
    set_if_present(enc, "target-usage", "7");

    // B-frames require the decoder to reorder frames, adding latency. Zero
    // means only I and P frames (no reorder delay).
    set_if_present(enc, "b-frames", "0");

    // Fewer reference frames reduces the DPB (decoded picture buffer) depth,
    // cutting end-to-end pipeline delay on both encoder and decoder.
    set_if_present(enc, "ref-frames", "1");

    // Macroblock-level bitrate control conflicts with strict CBR latency goals.
    set_if_present(enc, "mbbrc", "disabled");

    // x265enc equivalents: zerolatency removes all algorithmic buffering;
    // ultrafast minimises per-frame CPU work.
    set_if_present(enc, "tune", "zerolatency");
    set_if_present(enc, "speed-preset", "ultrafast");
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// `rebase_to_origin` zeroes the first frame and offsets the rest relative to
    /// it, however far in the future the source timestamps are.
    #[test]
    fn rebase_to_origin_zeroes_first_and_offsets_rest() {
        gst::init().unwrap();
        let base = AtomicU64::new(u64::MAX);

        // ~1000h, the running-time the shared encoder actually produced.
        let far = gst::ClockTime::from_seconds(3_600_000);

        let mut first = gst::Buffer::new();
        first.get_mut().unwrap().set_pts(far);
        rebase_to_origin(&mut first, &base);
        assert_eq!(first.pts(), Some(gst::ClockTime::ZERO));

        let mut later = gst::Buffer::new();
        later.get_mut().unwrap().set_pts(far + gst::ClockTime::from_mseconds(33));
        rebase_to_origin(&mut later, &base);
        assert_eq!(later.pts(), Some(gst::ClockTime::from_mseconds(33)));
    }

    /// End-to-end guard for the screen send branch: synthetic H265 fed with
    /// deliberately far-future timestamps (the original bug condition) must still
    /// produce RTP (byte-stream payloading) with re-based timestamps. Gated behind
    /// `HEARTH_CAPTURE` because it needs a real H265 encoder + GStreamer runtime.
    #[test]
    fn screen_send_branch_emits_rtp_with_rebased_timestamps() {
        if std::env::var("HEARTH_CAPTURE").unwrap_or_default().is_empty() {
            return;
        }
        gst::init().unwrap();

        let Some(samples) = generate_h265_aus(20) else {
            return; // no usable H265 encoder in this environment
        };

        let pipeline = gst::Pipeline::new();
        let fakesink = gst::ElementFactory::make("fakesink")
            .property("sync", false)
            .build()
            .unwrap();
        pipeline.add(&fakesink).unwrap();

        // fakesink stands in for webrtcbin; it accepts the application/x-rtp caps.
        let appsrc = build_screen_send_appsrc_branch(&pipeline, &fakesink, None).unwrap();

        let rtp_count = Arc::new(AtomicU64::new(0));
        let max_pts_ns = Arc::new(AtomicU64::new(0));
        {
            let rtp_count = rtp_count.clone();
            let max_pts_ns = max_pts_ns.clone();
            let sink_pad = fakesink.static_pad("sink").unwrap();
            sink_pad.add_probe(
                gst::PadProbeType::BUFFER | gst::PadProbeType::BUFFER_LIST,
                move |_, info| {
                    let (n, pts) = match &info.data {
                        Some(gst::PadProbeData::Buffer(b)) => (1, b.pts()),
                        Some(gst::PadProbeData::BufferList(l)) => {
                            (l.len() as u64, l.get(0).and_then(|b| b.pts()))
                        }
                        _ => (0, None),
                    };
                    rtp_count.fetch_add(n, Ordering::Relaxed);
                    if let Some(p) = pts {
                        max_pts_ns.fetch_max(p.nseconds(), Ordering::Relaxed);
                    }
                    gst::PadProbeReturn::Ok
                },
            );
        }

        pipeline.set_state(gst::State::Playing).unwrap();

        let far = gst::ClockTime::from_seconds(3_600_000);
        for (i, sample) in samples.iter().enumerate() {
            let mut buf = sample.copy();
            {
                let b = buf.get_mut().unwrap();
                let t = far + gst::ClockTime::from_mseconds(33 * i as u64);
                b.set_pts(t);
                b.set_dts(t);
            }
            let _ = appsrc.push_buffer(buf);
        }
        let _ = appsrc.end_of_stream();

        // Condition-based wait: poll until RTP appears or the budget expires.
        let mut waited_ms = 0;
        while rtp_count.load(Ordering::Relaxed) == 0 && waited_ms < 3000 {
            std::thread::sleep(std::time::Duration::from_millis(20));
            waited_ms += 20;
        }
        let _ = pipeline.set_state(gst::State::Null);

        assert!(
            rtp_count.load(Ordering::Relaxed) > 0,
            "rtph265pay produced no RTP (byte-stream payloading regressed?)"
        );
        assert!(
            max_pts_ns.load(Ordering::Relaxed) < gst::ClockTime::from_seconds(60).nseconds(),
            "far-future PTS reached the sink (timestamp re-base regressed?)"
        );
    }

    /// Encode `n` frames of `videotestsrc` to byte-stream H265 access units.
    /// Returns `None` when no H265 encoder is available.
    fn generate_h265_aus(n: u32) -> Option<Vec<gst::Buffer>> {
        let encoder = encoders::detect().0.unwrap_or("x265enc");
        let desc = format!(
            "videotestsrc num-buffers={n} ! video/x-raw,width=320,height=240,framerate=30/1 ! \
             videoconvert ! {encoder} ! h265parse config-interval=-1 ! \
             video/x-h265,stream-format=byte-stream,alignment=au ! appsink name=out sync=false"
        );
        let pipeline = gst::parse::launch(&desc).ok()?.downcast::<gst::Pipeline>().ok()?;
        let appsink = pipeline.by_name("out")?.downcast::<gst_app::AppSink>().ok()?;

        pipeline.set_state(gst::State::Playing).ok()?;

        let mut aus = Vec::new();
        while let Ok(sample) = appsink.pull_sample() {
            if let Some(buf) = sample.buffer() {
                aus.push(buf.copy());
            }
        }

        let _ = pipeline.set_state(gst::State::Null);

        (!aus.is_empty()).then_some(aus)
    }
}
