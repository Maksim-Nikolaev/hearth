mod app;
mod config;
mod theme;
mod ui;

use relm4::RelmApp;

fn main() {
    install_panic_logger();

    // The native low-latency backend (WASAPI IAudioClient3) exists only on
    // Windows; everywhere else voice always runs the GStreamer `voice_udp` path
    // (pulsesrc + Opus/RTP/UDP), and `HEARTH_GSTREAMER_VOICE` is a no-op.
    #[cfg(target_os = "windows")]
    let backend = if std::env::var_os("HEARTH_GSTREAMER_VOICE").is_some() {
        "GStreamer voice_udp (HEARTH_GSTREAMER_VOICE set)"
    } else {
        "native WASAPI IAudioClient3 + Opus (set HEARTH_GSTREAMER_VOICE=1 to revert)"
    };
    #[cfg(not(target_os = "windows"))]
    let backend = "GStreamer voice_udp (pulsesrc + Opus/RTP/UDP)";

    eprintln!("[hearth] voice backend: {backend}");

    setup_portable_runtime();
    ensure_gst_plugin_path();
    ensure_inprocess_plugin_scan();
    enable_latency_tracer();
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

/// Dev convenience: make the locally-built `gtk4paintablesink` (`gstgtk4`)
/// discoverable so in-window video works without the caller exporting
/// GST_PLUGIN_PATH. The plugin ships in `gst-plugins-rs`, not in the stock
/// GStreamer binaries, so it is built/installed out of band per platform.
fn ensure_gst_plugin_path() {
    let Some(dir) = hearth_plugin_dir().filter(|d| d.exists()) else { return };

    // Windows uses ';' as the GST_PLUGIN_PATH separator, Unix uses ':'.
    let sep = if cfg!(windows) { ';' } else { ':' };
    let existing = std::env::var("GST_PLUGIN_PATH").unwrap_or_default();
    let combined = if existing.is_empty() {
        dir.display().to_string()
    } else {
        format!("{}{}{}", dir.display(), sep, existing)
    };

    std::env::set_var("GST_PLUGIN_PATH", combined);
}

/// Opt-in deep latency profiling: `HEARTH_LATENCY_TRACE=1` turns on GStreamer's
/// built-in latency tracer (per-element and source→sink latency in the log).
/// Must run before `gstreamer::init`. The always-on per-hop `[latency]` lines
/// from the engine work without this; this is for element-level detail.
fn enable_latency_tracer() {
    if std::env::var_os("HEARTH_LATENCY_TRACE").is_some() {
        std::env::set_var("GST_TRACERS", "latency(flags=pipeline+element)");
        if std::env::var_os("GST_DEBUG").is_none() {
            std::env::set_var("GST_DEBUG", "GST_TRACER:7");
        }
    }
}

/// When running from a self-contained package — a `lib\gstreamer-1.0` folder
/// sits next to the executable — point GStreamer, gdk-pixbuf, and GSettings at
/// the bundled resources so nothing needs to be installed on the machine.
/// No-op in a dev build (no such folder) and on non-Windows.
#[cfg(target_os = "windows")]
fn setup_portable_runtime() {
    let Ok(exe) = std::env::current_exe() else { return };
    let Some(root) = exe.parent() else { return };

    let plugins = root.join("lib").join("gstreamer-1.0");
    if !plugins.exists() {
        return; // dev build, not a packaged layout
    }

    let set = |k: &str, v: std::path::PathBuf| {
        if std::env::var_os(k).is_none() {
            std::env::set_var(k, v);
        }
    };

    // Use only the bundled plugins; do not scan a machine-wide GStreamer install.
    set("GST_PLUGIN_SYSTEM_PATH", plugins.clone());
    set("GST_PLUGIN_PATH", plugins);
    set("GDK_PIXBUF_MODULE_FILE", root.join("lib/gdk-pixbuf-2.0/2.10.0/loaders.cache"));
    set("GSETTINGS_SCHEMA_DIR", root.join("share/glib-2.0/schemas"));
    // GST_REGISTRY is left to ensure_inprocess_plugin_scan (a writable per-user path).
}

#[cfg(not(target_os = "windows"))]
fn setup_portable_runtime() {}

/// Windows only: make `gtk4paintablesink` (`gstgtk4`) loadable.
///
/// Two GStreamer-on-Windows hazards, both rooted in GTK shipping a newer GLib
/// than the GStreamer binaries:
///
/// 1. The plugin scanner runs as `gst-plugin-scanner.exe`, which lives in the
///    GStreamer install and loads GStreamer's older GLib — too old for the
///    plugin, so it gets blacklisted. `GST_REGISTRY_FORK=no` scans in-process
///    instead, reusing the GLib the app already loaded (GTK's, since GTK leads
///    on PATH).
/// 2. The shared default registry can be poisoned by a prior failed scan (or by
///    other GStreamer tools), and the blacklist sticks while the file is
///    unchanged. A Hearth-owned registry keeps our good scan isolated.
///
/// No-op elsewhere.
#[cfg(target_os = "windows")]
fn ensure_inprocess_plugin_scan() {
    if std::env::var_os("GST_REGISTRY_FORK").is_none() {
        std::env::set_var("GST_REGISTRY_FORK", "no");
    }

    if std::env::var_os("GST_REGISTRY").is_none() {
        if let Some(base) = std::env::var_os("LOCALAPPDATA") {
            let mut reg = std::path::PathBuf::from(base);
            reg.push("hearth");
            let _ = std::fs::create_dir_all(&reg);
            reg.push("registry.bin");
            std::env::set_var("GST_REGISTRY", reg);
        }
    }
}

#[cfg(not(target_os = "windows"))]
fn ensure_inprocess_plugin_scan() {}

/// Linux/macOS: `~/.local/lib/hearth-gst-plugins`.
#[cfg(not(target_os = "windows"))]
fn hearth_plugin_dir() -> Option<std::path::PathBuf> {
    let mut dir = std::path::PathBuf::from(std::env::var_os("HOME")?);
    dir.push(".local/lib/hearth-gst-plugins");
    Some(dir)
}

/// Windows: a `gst-plugins\` folder next to the executable (packaged app) takes
/// precedence; otherwise `%LOCALAPPDATA%\hearth\gst-plugins` (dev).
#[cfg(target_os = "windows")]
fn hearth_plugin_dir() -> Option<std::path::PathBuf> {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(adjacent) = exe.parent().map(|p| p.join("gst-plugins")) {
            if adjacent.exists() {
                return Some(adjacent);
            }
        }
    }

    let mut dir = std::path::PathBuf::from(std::env::var_os("LOCALAPPDATA")?);
    dir.push("hearth");
    dir.push("gst-plugins");
    Some(dir)
}
