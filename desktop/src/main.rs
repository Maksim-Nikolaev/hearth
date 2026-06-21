mod app;
mod config;

use relm4::RelmApp;

fn main() {
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
    app.run::<app::AppModel>(config);
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
