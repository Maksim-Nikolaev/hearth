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
        "voicebench" => {
            // Device-independent: pure Opus + UDP software latency, no audio device.
            let secs = std::env::args().nth(2).and_then(|s| s.parse().ok()).unwrap_or(1.0);
            engine::audio::voicebench::run(secs)?;
        }
        #[cfg(target_os = "windows")]
        "wasapi3" => {
            // Phase 2 spike: IAudioClient3 low-latency loopback floor measurement.
            let secs = std::env::args().nth(2).and_then(|s| s.parse().ok()).unwrap_or(20);
            engine::audio::wasapi3::run_loopback(secs)?;
        }
        #[cfg(target_os = "windows")]
        "native" => {
            // Phase 2: exercise the NativeCapture + NativePlayback abstractions as
            // a mic -> speaker loopback (the building blocks for the voice path).
            let secs = std::env::args().nth(2).and_then(|s| s.parse().ok()).unwrap_or(20);
            let playback = std::sync::Arc::new(engine::audio::native::NativePlayback::start(None)?);
            let pb = playback.clone();
            let _capture = engine::audio::native::NativeCapture::start(None, move |mono| pb.push(0, mono))?;
            println!("[native] loopback up for {secs}s — mic -> speaker via NativeCapture/NativePlayback");
            std::thread::sleep(std::time::Duration::from_secs(secs));
            println!("[native] done.");
        }
        "share" | "view" | "call" | "listen" => {
            let share = matches!(mode.as_str(), "share" | "call");
            let flow = if matches!(mode.as_str(), "call" | "listen") {
                engine::flow::Flow::Voice
            } else {
                engine::flow::Flow::Screen
            };
            let http = std::env::var("HEARTH_HTTP").unwrap_or("http://127.0.0.1:8080".into());
            let ws = std::env::var("HEARTH_WS").unwrap_or("ws://127.0.0.1:8080".into());
            let user = std::env::var("HEARTH_USER").expect("HEARTH_USER");
            let pass = std::env::var("HEARTH_PASS").expect("HEARTH_PASS");
            let room = std::env::var("HEARTH_ROOM").unwrap_or("main".into());

            let cfg = engine::flow_peer::PeerConfig {
                http_base: &http,
                ws_base: &ws,
                username: &user,
                password: &pass,
                room: &room,
                share,
                flow,
                sink: engine::flow::VideoSink::Auto,
            };

            let rt = tokio::runtime::Runtime::new()?;
            rt.block_on(engine::flow_peer::run(cfg, None))?;
        }
        other => anyhow::bail!("unknown mode: {other}"),
    }

    Ok(())
}
