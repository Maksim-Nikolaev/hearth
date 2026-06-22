mod app;
mod config;
mod theme;
mod ui;

use relm4::RelmApp;

fn main() {
    install_panic_logger();

    ensure_gst_plugin_path();
    gstreamer::init().expect("init gstreamer");

    let config = config::Config::load();

    // A distinct app id per HEARTH_TITLE lets several instances run side by side
    // (GtkApplication is otherwise single-instance per id).
    let app_id = match std::env::var("HEARTH_TITLE") {
        Ok(t) if !t.is_empty() => {
            let suffix: String = t.chars().filter(|c| c.is_alphanumeric()).collect();
            format!("dev.hearth.desktop.{suffix}")
        }
        _ => "dev.hearth.desktop".into(),
    };

    let app = RelmApp::new(&app_id);
    theme::load();
    app.run::<app::AppModel>(config);
}

/// Log every panic with its thread, location, and a forced backtrace, then run
/// the default hook. A panic on a GStreamer streaming thread otherwise leaves
/// little trace before the process dies; this makes such failures diagnosable
/// regardless of the `RUST_BACKTRACE` setting.
fn install_panic_logger() {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let thread = std::thread::current();
        let name = thread.name().unwrap_or("<unnamed>");
        let backtrace = std::backtrace::Backtrace::force_capture();

        eprintln!("FATAL panic on thread '{name}': {info}\n{backtrace}");

        default(info);
    }));
}

/// Dev convenience: make the locally-built `gtk4paintablesink` discoverable so
/// in-window video works without the caller exporting GST_PLUGIN_PATH.
fn ensure_gst_plugin_path() {
    let Some(home) = std::env::var_os("HOME") else { return };

    let mut dir = std::path::PathBuf::from(home);
    dir.push(".local/lib/hearth-gst-plugins");
    if !dir.exists() {
        return;
    }

    let existing = std::env::var("GST_PLUGIN_PATH").unwrap_or_default();
    let combined = if existing.is_empty() {
        dir.display().to_string()
    } else {
        format!("{}:{}", dir.display(), existing)
    };

    std::env::set_var("GST_PLUGIN_PATH", combined);
}
