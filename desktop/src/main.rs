mod app;
mod config;

use relm4::RelmApp;

fn main() {
    ensure_gst_plugin_path();
    gstreamer::init().expect("init gstreamer");

    let config = config::Config::load();

    let app = RelmApp::new("dev.hearth.desktop");
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
