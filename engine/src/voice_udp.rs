//! Low-latency voice transport: raw RTP/Opus over plain UDP — no `webrtcbin`.
//!
//! `webrtcbin` adds ~150 ms (ICE + DTLS-SRTP + congestion control + a fat
//! jitter buffer). For a trusted P2P friend mesh we don't need any of that, so
//! the voice flow uses a thin pipeline per peer: one `udpsrc` to receive and one
//! `udpsink` to send, with a small `rtpjitterbuffer`. The shared
//! [`VoiceCapture`](crate::audio::capture::VoiceCapture) still owns the mic + DSP
//! and pushes PCM into `voice_appsrc`; this just transports it.
//!
//! Endpoints are exchanged out-of-band over the existing signaling channel
//! (Offer/Answer carry `"ip:port"` in place of an SDP for the Voice flow). No
//! ICE: direct UDP, which is correct for LAN/localhost. STUN hole-punching and
//! SRTP encryption are follow-ups (see `docs/research/voice-transport.md`).

use anyhow::{anyhow, Result};
use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use uuid::Uuid;

/// Attach a latency probe to an element's sink pad. For every buffer it computes
/// `now - PTS` (how long that audio has been in the pipeline up to this point)
/// and logs a rolling average every ~2 s, so each hop is a visible number as it
/// happens. The deltas between probe points are the per-stage cost.
fn probe_hop(element: &gst::Element, label: &'static str, pipeline: &gst::Pipeline, config_for: Option<Uuid>) {
    let Some(pad) = element.static_pad("sink") else { return };
    let weak = pipeline.downgrade();
    let count = Arc::new(AtomicU64::new(0));
    let sum_us = Arc::new(AtomicU64::new(0));
    // Query the configured latency once, on the first buffer — by then the
    // pipeline has finished its async latency calculation (querying at startup
    // returns live=false/min=0).
    let config_done = Arc::new(std::sync::atomic::AtomicBool::new(config_for.is_none()));

    pad.add_probe(gst::PadProbeType::BUFFER, move |_, info| {
        if let Some(gst::PadProbeData::Buffer(buf)) = info.data.as_ref() {
            if let (Some(pipeline), Some(pts)) = (weak.upgrade(), buf.pts()) {
                if !config_done.swap(true, Ordering::Relaxed) {
                    if let Some(target) = config_for {
                        report_configured_latency(&pipeline, target);
                    }
                }
                if let Some(now) = pipeline.current_running_time() {
                    // Skip the artifact where the frame-derived PTS timeline and
                    // the pipeline clock are offset (VoiceCapture restarted under
                    // a live pipeline): a real hop is never seconds long.
                    if now >= pts && (now - pts).mseconds() < 2000 {
                        sum_us.fetch_add((now - pts).useconds(), Ordering::Relaxed);
                        let n = count.fetch_add(1, Ordering::Relaxed) + 1;
                        if n % 200 == 0 {
                            let avg_ms = sum_us.swap(0, Ordering::Relaxed) as f64 / 200.0 / 1000.0;
                            count.store(0, Ordering::Relaxed);
                            eprintln!("[latency] {label}: {avg_ms:.1} ms");
                        }
                    }
                }
            }
        }
        gst::PadProbeReturn::Ok
    });
}

/// Log the pipeline's GStreamer-computed configured latency (sum of element
/// latencies — chiefly the jitter buffer + the audio sink).
fn report_configured_latency(pipeline: &gst::Pipeline, target: Uuid) {
    let mut q = gst::query::Latency::new();
    if pipeline.query(q.query_mut()) {
        let (live, min, max) = q.result();
        eprintln!(
            "[latency] voice {target}: configured live={live} min={:.1}ms max={}",
            min.mseconds() as f64,
            max.map(|m| format!("{:.1}ms", m.mseconds() as f64)).unwrap_or_else(|| "∞".into())
        );
    }
}

/// RTP payload type for the Opus voice stream (matches the legacy webrtc path).
const VOICE_PT: i32 = 97;

/// Playback sink for received voice. `sync=false` plays on arrival — unlike the
/// old webrtc path (whose internal buffer accumulated, forcing sync=true), the
/// explicit `rtpjitterbuffer` here already paces the stream, so clock-syncing
/// only adds the sink's render-ahead latency. `low-latency` keeps the WASAPI
/// device buffer minimal.
fn voice_playback_sink() -> Result<gst::Element> {
    #[cfg(target_os = "windows")]
    {
        Ok(gst::ElementFactory::make("wasapi2sink")
            .property("low-latency", true)
            .property("sync", false)
            .build()?)
    }
    #[cfg(not(target_os = "windows"))]
    {
        Ok(gst::ElementFactory::make("autoaudiosink")
            .property("sync", false)
            .build()?)
    }
}

/// Best-effort local IPv4 the peer can reach us on. Uses the route to a public
/// address to discover the LAN interface (no packet is sent). Falls back to
/// loopback, which still works for same-machine testing.
fn local_ip() -> String {
    std::net::UdpSocket::bind("0.0.0.0:0")
        .and_then(|s| {
            s.connect("8.8.8.8:80")?;
            s.local_addr()
        })
        .map(|a| a.ip().to_string())
        .unwrap_or_else(|_| "127.0.0.1".to_string())
}

/// One peer's bidirectional voice transport. Built immediately (the receiver
/// starts listening on an ephemeral port); the sender's destination is filled in
/// by [`set_remote`] once the peer's endpoint arrives over signaling.
pub(crate) struct VoiceUdpPeer {
    pipeline: gst::Pipeline,
    #[allow(dead_code)] // kept for diagnostics / future per-peer logging
    target: Uuid,
    local_port: u16,
    udpsink: gst::Element,
    send_valve: gst::Element,
    jitter: gst::Element,
    voice_appsrc: gst_app::AppSrc,
    spk_valve: gst::Element,
    spk_volume: gst::Element,
}

impl VoiceUdpPeer {
    pub fn new(target: Uuid) -> Result<Self> {
        gst::init()?;
        let pipeline = gst::Pipeline::new();

        // ── recv: udpsrc(ephemeral) → jitterbuffer → depay → opusdec → sink ──
        let rtp_caps = gst::Caps::builder("application/x-rtp")
            .field("media", "audio")
            .field("clock-rate", 48000i32)
            .field("encoding-name", "OPUS")
            .field("payload", VOICE_PT)
            .build();
        // Allocate a free UDP port up front (bind/drop), then bind udpsrc to it.
        // Avoids reading udpsrc's `used-port` after PLAYING (whose property type
        // panicked, and a panic there can't unwind the live pipeline cleanly).
        let local_port = std::net::UdpSocket::bind("0.0.0.0:0")
            .and_then(|s| s.local_addr())
            .map(|a| a.port())
            .map_err(|e| anyhow!("could not allocate a voice port: {e}"))?;
        let udpsrc = gst::ElementFactory::make("udpsrc")
            .property("port", local_port as i32)
            .property("caps", &rtp_caps)
            .build()?;
        let jitter = gst::ElementFactory::make("rtpjitterbuffer")
            // Shared with the Voice-settings "Jitter buffer (ms)" slider.
            .property("latency", crate::flow_peer::jitter_latency_ms())
            .property("do-lost", true)
            .build()?;
        let depay = gst::ElementFactory::make("rtpopusdepay").build()?;
        let dec = gst::ElementFactory::make("opusdec")
            .property("plc", true) // packet-loss concealment
            .build()?;
        let rconv = gst::ElementFactory::make("audioconvert").build()?;
        let rresample = gst::ElementFactory::make("audioresample").build()?;
        // Master speaker volume for this peer (user output-volume slider). Live.
        let spk_volume = gst::ElementFactory::make("volume")
            .name("spk_volume")
            .property("volume", 1.0f64)
            .build()?;
        // deafen gate: drop=true silences incoming audio without tearing down.
        let spk_valve = gst::ElementFactory::make("valve")
            .name("spk_valve")
            .property("drop", false)
            .build()?;
        let sink = voice_playback_sink()?;

        pipeline.add_many([&udpsrc, &jitter, &depay, &dec, &rconv, &rresample, &spk_volume, &spk_valve, &sink])?;
        gst::Element::link_many([&udpsrc, &jitter, &depay, &dec, &rconv, &rresample, &spk_volume, &spk_valve, &sink])?;

        // ── send: appsrc(PCM) → opusenc → rtpopuspay → udpsink ──
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
            // PTS is frame-derived in VoiceCapture (perfectly 10 ms-spaced).
            // Do NOT use do-timestamp here: stamping on push captures the
            // appsink callback's burst jitter, which the receiver's jitter
            // buffer then absorbs — measured +60 ms. Clean paced timestamps keep
            // it tight. (The send probe clamps the post-restart PTS artifact.)
            .build();
        let sconv = gst::ElementFactory::make("audioconvert").build()?;
        let sresample = gst::ElementFactory::make("audioresample").build()?;
        // Lowest-latency Opus: restricted-lowdelay drops the SILK layer + variable
        // lookahead (~26 ms → ~5 ms algorithmic delay); 10 ms frames match the DSP
        // frame and halve packetization delay vs the 20 ms default. inband-fec
        // lets the decoder conceal a lost packet from the next one.
        let enc = gst::ElementFactory::make("opusenc")
            .property_from_str("audio-type", "restricted-lowdelay")
            .property_from_str("frame-size", "10")
            .property("inband-fec", true)
            .build()?;
        let pay = gst::ElementFactory::make("rtpopuspay")
            .property("pt", VOICE_PT as u32)
            .build()?;
        // Gate the sender shut until we know the peer's endpoint. Otherwise
        // packets would fire at the placeholder address — and on Windows a UDP
        // send to a dead local port returns WSAECONNRESET, which errors the sink.
        let send_valve = gst::ElementFactory::make("valve")
            .name("send_valve")
            .property("drop", true)
            .build()?;
        // host/port are placeholders until set_remote(); sync/async off so the
        // live send branch never waits on a clock or preroll.
        let udpsink = gst::ElementFactory::make("udpsink")
            .property("host", "127.0.0.1")
            .property("port", 9i32) // discard port; nothing flows until the valve opens
            .property("sync", false)
            .property("async", false)
            .build()?;

        pipeline.add_many([appsrc.upcast_ref(), &sconv, &sresample, &enc, &pay, &send_valve, &udpsink])?;
        gst::Element::link_many([appsrc.upcast_ref(), &sconv, &sresample, &enc, &pay, &send_valve, &udpsink])?;

        // Surface pipeline errors instead of a silent stall.
        if let Some(bus) = pipeline.bus() {
            let watch = bus.add_watch(move |_, msg| {
                if let gst::MessageView::Error(e) = msg.view() {
                    eprintln!(
                        "voice udp error from {:?}: {} ({:?})",
                        e.src().map(|s| s.path_string()),
                        e.error(),
                        e.debug()
                    );
                }
                gst::glib::ControlFlow::Continue
            });
            if let Ok(w) = watch {
                std::mem::forget(w);
            }
        }

        // Go live. On failure, set NULL first — a `udpsrc` left attached to the
        // main context and then finalized aborts the process (GSocket finalize
        // assertion).
        if let Err(e) = pipeline.set_state(gst::State::Playing) {
            let _ = pipeline.set_state(gst::State::Null);
            let _ = pipeline.state(gst::ClockTime::from_seconds(2));
            return Err(anyhow!("voice pipeline failed to start: {e:?}"));
        }

        // Per-hop latency instrumentation (always on; ~one line / 2 s per hop).
        // Send PTS is the frame-capture time, so this is mic capture -> wire
        // (DSP frame + encode + pay). The recv probe also logs the configured
        // latency once it settles.
        probe_hop(&udpsink, "voice send  (mic -> wire)", &pipeline, None);
        probe_hop(&sink, "voice recv  (wire -> speaker, post-jitter)", &pipeline, Some(target));

        Ok(Self {
            pipeline,
            target,
            local_port,
            udpsink,
            send_valve,
            jitter,
            voice_appsrc: appsrc,
            spk_valve,
            spk_volume,
        })
    }

    /// Master speaker volume for this peer's playback (0.0–1.0). Live.
    pub fn set_output_volume(&self, v: f64) {
        self.spk_volume.set_property("volume", v);
    }

    /// Live-update the receive jitter buffer depth (ms). Lets the Voice-settings
    /// slider change buffering on an active call so its latency effect is
    /// testable without rejoining.
    pub fn set_jitter_ms(&self, ms: u32) {
        self.jitter.set_property("latency", ms);
    }

    /// The `ip:port` we receive on — carried to the peer in the Offer/Answer.
    pub fn local_endpoint(&self) -> String {
        format!("{}:{}", local_ip(), self.local_port)
    }

    /// Point the sender at the peer's `ip:port` (from their Offer/Answer).
    pub fn set_remote(&self, endpoint: &str) -> Result<()> {
        let (host, port) = endpoint
            .rsplit_once(':')
            .ok_or_else(|| anyhow!("malformed voice endpoint: {endpoint}"))?;
        let port: i32 = port.parse()?;
        self.udpsink.set_property("host", host);
        self.udpsink.set_property("port", port);
        // Endpoint known — let the sender flow.
        self.send_valve.set_property("drop", false);
        Ok(())
    }

    /// The `appsrc` the shared `VoiceCapture` pushes DSP'd mic frames into.
    pub fn voice_appsrc(&self) -> gst_app::AppSrc {
        self.voice_appsrc.clone()
    }

    /// Deafen: drop incoming audio without tearing the flow down.
    pub fn set_deaf(&self, on: bool) {
        self.spk_valve.set_property("drop", on);
    }
}

impl Drop for VoiceUdpPeer {
    fn drop(&mut self) {
        // Block until NULL is actually reached so `udpsrc` removes its socket
        // source before the pipeline is finalized — otherwise GSocket finalize
        // asserts and aborts the process.
        let _ = self.pipeline.set_state(gst::State::Null);
        let _ = self.pipeline.state(gst::ClockTime::from_seconds(2));
    }
}
