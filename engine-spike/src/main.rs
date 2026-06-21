mod encoders;
mod pipeline;

fn main() -> anyhow::Result<()> {
    gstreamer::init()?;

    let mode = std::env::args().nth(1).unwrap_or_else(|| "probe".into());

    match mode.as_str() {
        "probe" => {
            let (chosen, list) = encoders::detect();

            for (factory, label, ok) in &list {
                println!("[{}] {:<14} {}", if *ok { "x" } else { " " }, factory, label);
            }

            println!("\nselected encoder: {:?}", chosen);
        }
        "local" => pipeline::run_local()?,
        "offer" => pipeline::run_peer(true)?,
        "answer" => pipeline::run_peer(false)?,
        other => anyhow::bail!("unknown mode: {other}"),
    }

    Ok(())
}
