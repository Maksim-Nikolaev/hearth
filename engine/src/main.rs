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
        other => anyhow::bail!("unknown mode: {other}"),
    }

    Ok(())
}
