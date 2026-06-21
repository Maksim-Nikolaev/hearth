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
        "share" | "view" => {
            let share = mode == "share";
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
                flow: engine::flow::Flow::Screen,
                sink: engine::flow::VideoSink::Auto,
            };

            let rt = tokio::runtime::Runtime::new()?;
            rt.block_on(engine::flow_peer::run(cfg, None))?;
        }
        other => anyhow::bail!("unknown mode: {other}"),
    }

    Ok(())
}
