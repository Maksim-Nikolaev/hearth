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
use uuid::Uuid;

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
    voice_appsrc: gst_app::AppSrc,
    spk_valve: gst::Element,
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
        // deafen gate: drop=true silences incoming audio without tearing down.
        let spk_valve = gst::ElementFactory::make("valve")
            .name("spk_valve")
            .property("drop", false)
            .build()?;
        let sink = voice_playback_sink()?;

        pipeline.add_many([&udpsrc, &jitter, &depay, &dec, &rconv, &rresample, &spk_valve, &sink])?;
        gst::Element::link_many([&udpsrc, &jitter, &depay, &dec, &rconv, &rresample, &spk_valve, &sink])?;

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

        Ok(Self {
            pipeline,
            target,
            local_port,
            udpsink,
            send_valve,
            voice_appsrc: appsrc,
            spk_valve,
        })
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
