use anyhow::Result;
use gstreamer as gst;
use gstreamer::glib;
use gstreamer::prelude::*;
use gstreamer_sdp as gst_sdp;
use gstreamer_webrtc as gst_webrtc;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;
use std::time::Duration;

/// Single-process pipeline: capture + HW encode + decode + display on one machine.
/// Isolates the capture/encode half from the network half.
pub fn run_local() -> Result<()> {
    let encoder = crate::encoders::detect().0.unwrap_or("x265enc");

    let desc = format!(
        "ximagesrc use-damage=false ! videoconvert ! {encoder} ! h265parse ! avdec_h265 ! videoconvert ! autovideosink sync=false"
    );
    println!("pipeline: {desc}");

    let pipeline = gst::parse::launch(&desc)?;
    pipeline.set_state(gst::State::Playing)?;

    let bus = pipeline.bus().unwrap();

    for msg in bus.iter_timed(gst::ClockTime::NONE) {
        use gst::MessageView::*;

        match msg.view() {
            Eos(_) => break,
            Error(e) => {
                eprintln!("error: {} ({:?})", e.error(), e.debug());
                break;
            }
            _ => {}
        }
    }

    pipeline.set_state(gst::State::Null)?;

    Ok(())
}

// File-based signaling channel (throwaway; real signaling arrives in M3).
const OFFER: &str = "/tmp/hearth_offer.sdp";
const ANSWER: &str = "/tmp/hearth_answer.sdp";
const ICE_FROM_OFFER: &str = "/tmp/hearth_ice_offer.txt";
const ICE_FROM_ANSWER: &str = "/tmp/hearth_ice_answer.txt";

/// Two-peer screenshare over `webrtcbin`. The offerer captures and sends this
/// machine's screen; the answerer decodes and displays it. SDP + ICE are
/// exchanged through files in /tmp so no signaling server is needed.
pub fn run_peer(is_offerer: bool) -> Result<()> {
    let encoder = crate::encoders::detect().0.unwrap_or("x265enc");

    // Each side owns (and resets) only the files it writes.
    let (sdp_out, sdp_in, ice_out, ice_in) = if is_offerer {
        (OFFER, ANSWER, ICE_FROM_OFFER, ICE_FROM_ANSWER)
    } else {
        (ANSWER, OFFER, ICE_FROM_ANSWER, ICE_FROM_OFFER)
    };
    let _ = std::fs::remove_file(sdp_out);
    let _ = std::fs::remove_file(ice_out);

    let pipeline = gst::Pipeline::new();
    let webrtc = gst::ElementFactory::make("webrtcbin")
        .name("wrtc")
        .property_from_str("stun-server", "stun://stun.l.google.com:19302")
        .build()?;
    pipeline.add(&webrtc)?;

    if is_offerer {
        let src = gst::ElementFactory::make("ximagesrc").property("use-damage", false).build()?;
        let conv = gst::ElementFactory::make("videoconvert").build()?;
        let enc = gst::ElementFactory::make(encoder).build()?;
        let parse = gst::ElementFactory::make("h265parse").build()?;
        let pay = gst::ElementFactory::make("rtph265pay").property("config-interval", -1i32).build()?;
        let caps = gst::ElementFactory::make("capsfilter")
            .property("caps", gst::Caps::builder("application/x-rtp")
                .field("media", "video")
                .field("encoding-name", "H265")
                .field("payload", 96i32)
                .build())
            .build()?;

        pipeline.add_many([&src, &conv, &enc, &parse, &pay, &caps])?;
        gst::Element::link_many([&src, &conv, &enc, &parse, &pay, &caps])?;
        caps.link(&webrtc)?;
    }

    // Incoming media (answerer side): decode + display.
    let pipeline_weak = pipeline.downgrade();
    webrtc.connect_pad_added(move |_wrtc, pad| {
        if pad.direction() != gst::PadDirection::Src {
            return;
        }

        let Some(pipeline) = pipeline_weak.upgrade() else { return };

        let depay = gst::ElementFactory::make("rtph265depay").build().unwrap();
        let parse = gst::ElementFactory::make("h265parse").build().unwrap();
        let dec = gst::ElementFactory::make("avdec_h265").build().unwrap();
        let conv = gst::ElementFactory::make("videoconvert").build().unwrap();
        let sink = gst::ElementFactory::make("autovideosink").property("sync", false).build().unwrap();

        pipeline.add_many([&depay, &parse, &dec, &conv, &sink]).unwrap();
        gst::Element::link_many([&depay, &parse, &dec, &conv, &sink]).unwrap();

        for e in [&depay, &parse, &dec, &conv, &sink] {
            e.sync_state_with_parent().unwrap();
        }

        pad.link(&depay.static_pad("sink").unwrap()).unwrap();
        println!("incoming stream linked -> decoding + displaying");
    });

    // Emit our local ICE candidates to the peer's inbound file.
    let ice_out_owned = ice_out.to_string();
    webrtc.connect("on-ice-candidate", false, move |vals| {
        let mline = vals[1].get::<u32>().unwrap();
        let cand = vals[2].get::<String>().unwrap();

        append_line(&ice_out_owned, &format!("{mline}\t{cand}"));

        None
    });

    // Report connection progress; "Connected" is the loopback success signal.
    webrtc.connect_notify(Some("connection-state"), |wrtc, _| {
        let state = wrtc.property::<gst_webrtc::WebRTCPeerConnectionState>("connection-state");
        println!("connection-state: {state:?}");
    });
    webrtc.connect_notify(Some("ice-connection-state"), |wrtc, _| {
        let state = wrtc.property::<gst_webrtc::WebRTCICEConnectionState>("ice-connection-state");
        println!("ice-connection-state: {state:?}");
    });

    // Offerer kicks off negotiation as soon as the pipeline is live.
    if is_offerer {
        let webrtc_weak = webrtc.downgrade();
        webrtc.connect("on-negotiation-needed", false, move |_vals| {
            let webrtc = webrtc_weak.upgrade()?;
            let wc = webrtc.clone();

            let promise = gst::Promise::with_change_func(move |reply| {
                let Ok(Some(reply)) = reply else { return };
                let offer = reply.value("offer").unwrap().get::<gst_webrtc::WebRTCSessionDescription>().unwrap();

                wc.emit_by_name::<()>("set-local-description", &[&offer, &None::<gst::Promise>]);
                std::fs::write(OFFER, offer.sdp().as_text().unwrap()).unwrap();
                println!("offer written");
            });

            webrtc.emit_by_name::<()>("create-offer", &[&None::<gst::Structure>, &promise]);
            None
        });
    }

    pipeline.set_state(gst::State::Playing)?;

    // Signaling runs off the main thread so the GLib main loop can drive webrtcbin.
    let webrtc_sig = webrtc.clone();
    let sdp_in_owned = sdp_in.to_string();
    let ice_in_owned = ice_in.to_string();
    std::thread::spawn(move || {
        let sdp_text = wait_read(&sdp_in_owned);
        let sdp = gst_sdp::SDPMessage::parse_buffer(sdp_text.as_bytes()).unwrap();

        if is_offerer {
            let answer = gst_webrtc::WebRTCSessionDescription::new(gst_webrtc::WebRTCSDPType::Answer, sdp);
            webrtc_sig.emit_by_name::<()>("set-remote-description", &[&answer, &None::<gst::Promise>]);
        } else {
            let offer = gst_webrtc::WebRTCSessionDescription::new(gst_webrtc::WebRTCSDPType::Offer, sdp);
            webrtc_sig.emit_by_name::<()>("set-remote-description", &[&offer, &None::<gst::Promise>]);

            let wc = webrtc_sig.clone();
            let promise = gst::Promise::with_change_func(move |reply| {
                let Ok(Some(reply)) = reply else { return };
                let answer = reply.value("answer").unwrap().get::<gst_webrtc::WebRTCSessionDescription>().unwrap();

                wc.emit_by_name::<()>("set-local-description", &[&answer, &None::<gst::Promise>]);
                std::fs::write(ANSWER, answer.sdp().as_text().unwrap()).unwrap();
                println!("answer written");
            });
            webrtc_sig.emit_by_name::<()>("create-answer", &[&None::<gst::Structure>, &promise]);
        }

        poll_ice(&ice_in_owned, &webrtc_sig);
    });

    let main_loop = glib::MainLoop::new(None, false);

    let ml = main_loop.clone();
    let bus = pipeline.bus().unwrap();
    let _watch = bus.add_watch(move |_bus, msg| {
        use gst::MessageView::*;

        match msg.view() {
            Eos(_) => ml.quit(),
            Error(e) => {
                eprintln!("error: {} ({:?})", e.error(), e.debug());
                ml.quit();
            }
            _ => {}
        }

        glib::ControlFlow::Continue
    })?;

    main_loop.run();

    pipeline.set_state(gst::State::Null)?;

    Ok(())
}

fn append_line(path: &str, line: &str) {
    let mut f = OpenOptions::new().create(true).append(true).open(path).unwrap();
    writeln!(f, "{line}").unwrap();
}

/// Block until `path` exists and is non-empty, then read it.
fn wait_read(path: &str) -> String {
    loop {
        if Path::new(path).exists() {
            if let Ok(s) = std::fs::read_to_string(path) {
                if !s.trim().is_empty() {
                    return s;
                }
            }
        }

        std::thread::sleep(Duration::from_millis(200));
    }
}

/// Poll the peer's ICE file, feeding each new candidate to webrtcbin.
fn poll_ice(path: &str, webrtc: &gst::Element) {
    let mut consumed = 0usize;

    loop {
        if let Ok(content) = std::fs::read_to_string(path) {
            let lines: Vec<&str> = content.lines().collect();

            for line in lines.iter().skip(consumed) {
                if let Some((mline, cand)) = line.split_once('\t') {
                    let mline: u32 = mline.parse().unwrap_or(0);
                    webrtc.emit_by_name::<()>("add-ice-candidate", &[&mline, &cand.to_string()]);
                }
            }

            consumed = lines.len();
        }

        std::thread::sleep(Duration::from_millis(200));
    }
}
