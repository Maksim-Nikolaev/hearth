use std::path::PathBuf;

use serde::{Deserialize, Serialize};

const SERVICE: &str = "hearth";
const TOKEN_USER: &str = "access-token";

// ── Settings enums ────────────────────────────────────────────────────────────
// Desktop-local mirrors of engine enums; kept plain + serde so the UI layer
// can own them without pulling engine types into serde territory.

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum NsLevel {
    Off,
    Low,
    #[default]
    Moderate,
    High,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ActivationKind {
    #[default]
    Voice,
    PushToTalk,
    AlwaysOn,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ContentKind {
    #[default]
    Smoothness,
    Clarity,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ShareAudioKind {
    #[default]
    None,
    System,
    App,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum VoiceProfile {
    #[default]
    Custom,
    Headset,
    Speaker,
    Auto,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum AecMethodKind {
    #[default]
    Speex,
    Webrtc,
}

// ── Settings ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Settings {
    pub input_device: Option<String>,
    pub output_device: Option<String>,
    pub input_volume: f64,
    pub output_volume: f64,
    pub noise_suppression: NsLevel,
    pub echo_cancellation: bool,
    /// Echo-canceller algorithm for the native path (Speex / WebRTC). Only
    /// applies when `echo_cancellation` is on. `serde(default)` keeps older saved
    /// settings loadable.
    #[serde(default = "default_aec_method")]
    pub aec_method: AecMethodKind,
    /// Residual-echo suppression strength (0–100) for the native speex AEC.
    /// `serde(default)` keeps older saved settings loadable.
    #[serde(default = "default_echo_strength")]
    pub echo_cancel_strength: u8,
    pub agc: bool,
    pub vad: bool,
    pub input_sensitivity: f32,
    pub activation: ActivationKind,
    pub ptt_key: Option<String>,
    pub share_width: u32,
    pub share_height: u32,
    pub share_fps: u32,
    pub share_content: ContentKind,
    pub share_audio: ShareAudioKind,
    pub share_bitrate_kbps: u32,
    /// Jitter-buffer depth in ms (lower = less latency). Applied live to active
    /// UDP voice and to connections established after a change. `serde(default)`
    /// keeps older saved settings loadable. 20 ms = ~2 Opus packets of headroom,
    /// the measured stability floor on LAN (10 ms destabilizes).
    #[serde(default = "default_jitter_ms")]
    pub jitter_latency_ms: u32,
    #[serde(default)]
    pub profile: VoiceProfile,
}

fn default_jitter_ms() -> u32 {
    20
}

fn default_echo_strength() -> u8 {
    engine::audio::dsp::DEFAULT_ECHO_STRENGTH
}

/// WebRTC AEC3 is the default where it's available; Windows can't build it, so
/// it falls back to Speex there.
fn default_aec_method() -> AecMethodKind {
    if cfg!(target_os = "windows") {
        AecMethodKind::Speex
    } else {
        AecMethodKind::Webrtc
    }
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            input_device: None,
            output_device: None,
            input_volume: 1.0,
            output_volume: 1.0,
            noise_suppression: NsLevel::Off,
            echo_cancellation: false,
            aec_method: default_aec_method(),
            echo_cancel_strength: default_echo_strength(),
            agc: false,
            vad: false,
            input_sensitivity: -40.0,
            activation: ActivationKind::Voice,
            ptt_key: None,
            share_width: 1920,
            share_height: 1080,
            share_fps: 30,
            share_content: ContentKind::Smoothness,
            share_audio: ShareAudioKind::None,
            share_bitrate_kbps: 6000,
            jitter_latency_ms: default_jitter_ms(),
            profile: VoiceProfile::Custom,
        }
    }
}

fn to_engine_ns(n: NsLevel) -> engine::audio::dsp::NsLevel {
    use engine::audio::dsp::NsLevel as E;
    match n {
        NsLevel::Off => E::Off,
        NsLevel::Low => E::Low,
        NsLevel::Moderate => E::Moderate,
        NsLevel::High => E::High,
    }
}

fn from_engine_ns(n: engine::audio::dsp::NsLevel) -> NsLevel {
    use engine::audio::dsp::NsLevel as E;
    match n {
        E::Off => NsLevel::Off,
        E::Low => NsLevel::Low,
        E::Moderate => NsLevel::Moderate,
        E::High => NsLevel::High,
    }
}

fn to_engine_aec(m: AecMethodKind) -> engine::audio::dsp::AecMethod {
    match m {
        AecMethodKind::Speex => engine::audio::dsp::AecMethod::Speex,
        AecMethodKind::Webrtc => engine::audio::dsp::AecMethod::Webrtc,
    }
}

fn from_engine_aec(m: engine::audio::dsp::AecMethod) -> AecMethodKind {
    match m {
        engine::audio::dsp::AecMethod::Speex => AecMethodKind::Speex,
        engine::audio::dsp::AecMethod::Webrtc => AecMethodKind::Webrtc,
    }
}

fn to_engine_profile(p: VoiceProfile) -> engine::audio::profile::VoiceProfile {
    use engine::audio::profile::VoiceProfile as E;
    match p {
        VoiceProfile::Custom => E::Custom,
        VoiceProfile::Headset => E::Headset,
        VoiceProfile::Speaker => E::Speaker,
        VoiceProfile::Auto => E::Auto,
    }
}

/// The user's custom slot, as an engine `DspConfig` (the stored flag fields).
pub fn settings_custom_dsp(s: &Settings) -> engine::audio::dsp::DspConfig {
    engine::audio::dsp::DspConfig {
        echo_cancel: s.echo_cancellation,
        aec_method: to_engine_aec(s.aec_method),
        echo_cancel_strength: s.echo_cancel_strength,
        noise_suppression: to_engine_ns(s.noise_suppression),
        agc: s.agc,
        vad: s.vad,
        high_pass: true,
    }
}

/// Write an engine `DspConfig` back into the flag fields (display + demote).
pub fn write_dsp(s: &mut Settings, d: &engine::audio::dsp::DspConfig) {
    s.echo_cancellation = d.echo_cancel;
    s.aec_method = from_engine_aec(d.aec_method);
    s.echo_cancel_strength = d.echo_cancel_strength;
    s.noise_suppression = from_engine_ns(d.noise_suppression);
    s.agc = d.agc;
    s.vad = d.vad;
}

/// The effective `DspConfig` for the current profile + classification.
pub fn effective_dsp(s: &Settings, output: engine::audio::profile::OutputKind)
    -> engine::audio::dsp::DspConfig
{
    engine::audio::profile::effective(to_engine_profile(s.profile), &settings_custom_dsp(s), output)
}

/// Apply the "editing a preset demotes to Custom" rule: materialize the current
/// effective config into the flag fields, then switch the profile to Custom. The
/// caller applies the user's single edit after this (or before, on the widget).
pub fn demote_to_custom(s: &mut Settings, output: engine::audio::profile::OutputKind) {
    let eff = effective_dsp(s, output);
    write_dsp(s, &eff);
    s.profile = VoiceProfile::Custom;
}

/// Server endpoints plus token persistence (OS keyring, with a file/env fallback
/// when no Secret Service is available).
pub struct Config {
    pub http: String,
    pub ws: String,
}

impl Config {
    pub fn load() -> Self {
        Config {
            http: std::env::var("HEARTH_HTTP").unwrap_or_else(|_| "http://127.0.0.1:8080".into()),
            ws: std::env::var("HEARTH_WS").unwrap_or_else(|_| "ws://127.0.0.1:8080".into()),
        }
    }

    pub fn save_token(&self, token: &str) {
        if keyring::Entry::new(SERVICE, TOKEN_USER)
            .and_then(|e| e.set_password(token))
            .is_ok()
        {
            return;
        }

        if let Some(path) = token_file() {
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let _ = std::fs::write(path, token);
        }
    }

    pub fn load_token(&self) -> Option<String> {
        if let Ok(t) = std::env::var("HEARTH_TOKEN") {
            if !t.is_empty() {
                return Some(t);
            }
        }

        if let Ok(t) = keyring::Entry::new(SERVICE, TOKEN_USER).and_then(|e| e.get_password()) {
            return Some(t);
        }

        token_file()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    }

    pub fn load_settings(&self) -> Settings {
        let Some(path) = settings_file() else {
            return Settings::default();
        };

        let Ok(text) = std::fs::read_to_string(path) else {
            return Settings::default();
        };

        serde_json::from_str(&text).unwrap_or_default()
    }

    pub fn save_settings(&self, settings: &Settings) {
        let Some(path) = settings_file() else {
            return;
        };

        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }

        if let Ok(json) = serde_json::to_string_pretty(settings) {
            let _ = std::fs::write(path, json);
        }
    }
}

/// Host:port to pre-fill the login Server field: the saved value, else the
/// `HEARTH_HTTP` host (env seed for dev), else localhost.
pub fn initial_server() -> String {
    if let Some(s) = saved_server() {
        return s;
    }
    if let Ok(http) = std::env::var("HEARTH_HTTP") {
        let host = host_of(&http);
        if !host.is_empty() {
            return host;
        }
    }
    "127.0.0.1:8080".to_string()
}

/// The persisted login server (host:port), if any.
pub fn saved_server() -> Option<String> {
    server_file()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Remember the login server so the next launch pre-fills it.
pub fn save_server(server: &str) {
    let Some(path) = server_file() else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(path, server.trim());
}

/// Derive the HTTP and WS base URLs from a `host:port`. Any scheme the user
/// typed is stripped; LAN / private-network mode uses plain `http`/`ws`.
pub fn endpoints_from_server(server: &str) -> (String, String) {
    let host = host_of(server);
    (format!("http://{host}"), format!("ws://{host}"))
}

/// Strip any URL scheme and trailing slash, leaving the bare `host:port`.
fn host_of(s: &str) -> String {
    let s = s.trim().trim_end_matches('/');
    for scheme in ["https://", "http://", "wss://", "ws://"] {
        if let Some(rest) = s.strip_prefix(scheme) {
            return rest.to_string();
        }
    }
    s.to_string()
}

fn server_file() -> Option<PathBuf> {
    directories::ProjectDirs::from("dev", "hearth", "hearth").map(|d| d.config_dir().join("server"))
}

fn token_file() -> Option<PathBuf> {
    directories::ProjectDirs::from("dev", "hearth", "hearth").map(|d| d.config_dir().join("token"))
}

fn settings_file() -> Option<PathBuf> {
    directories::ProjectDirs::from("dev", "hearth", "hearth")
        .map(|d| d.config_dir().join("settings.json"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoints_derive_http_and_ws_and_strip_scheme() {
        assert_eq!(
            endpoints_from_server("192.168.178.35:8080"),
            ("http://192.168.178.35:8080".into(), "ws://192.168.178.35:8080".into()),
        );
        // A pasted scheme or trailing slash is tolerated.
        assert_eq!(
            endpoints_from_server("  http://host:9/ "),
            ("http://host:9".into(), "ws://host:9".into()),
        );
        assert_eq!(
            endpoints_from_server("ws://host:9"),
            ("http://host:9".into(), "ws://host:9".into()),
        );
    }

    #[test]
    fn settings_round_trip_via_json() {
        let s = Settings { input_sensitivity: -42.0, agc: true, ..Default::default() };
        let json = serde_json::to_string(&s).unwrap();
        let back: Settings = serde_json::from_str(&json).unwrap();

        assert_eq!(back.input_sensitivity, -42.0);
        assert!(back.agc);
    }

    #[test]
    fn defaults_are_sane() {
        let s = Settings::default();

        assert_eq!(s.share_fps, 30);
        assert_eq!(s.share_bitrate_kbps, 6000);
        assert!(matches!(s.activation, ActivationKind::Voice));
    }

    #[test]
    fn profile_defaults_to_custom_and_round_trips() {
        let s = Settings::default();
        assert!(matches!(s.profile, VoiceProfile::Custom));
        let json = serde_json::to_string(&s).unwrap();
        let back: Settings = serde_json::from_str(&json).unwrap();
        assert!(matches!(back.profile, VoiceProfile::Custom));
    }

    #[test]
    fn old_settings_without_profile_load_as_custom() {
        // A settings blob saved before `profile` existed must still load.
        let json = r#"{"input_device":null,"output_device":null,"input_volume":1.0,
            "output_volume":1.0,"noise_suppression":"off","echo_cancellation":false,
            "agc":false,"vad":false,"input_sensitivity":-40.0,"activation":"voice",
            "ptt_key":null,"share_width":1920,"share_height":1080,"share_fps":30,
            "share_content":"smoothness","share_audio":"none","share_bitrate_kbps":6000}"#;
        let s: Settings = serde_json::from_str(json).unwrap();
        assert!(matches!(s.profile, VoiceProfile::Custom));
    }

    #[test]
    fn demote_materializes_preset_then_becomes_custom() {
        // On Headset, the stored custom flags are all-off, but demoting must write
        // the Headset effective (AEC off, NS moderate, AGC on) into the flags.
        let mut s = Settings { profile: VoiceProfile::Headset, ..Default::default() };
        demote_to_custom(&mut s, engine::audio::profile::OutputKind::Unknown);
        assert!(matches!(s.profile, VoiceProfile::Custom));
        assert!(s.agc);                 // from the Headset preset
        assert!(!s.echo_cancellation);  // Headset = AEC off
        assert!(matches!(s.noise_suppression, NsLevel::Moderate));
    }

    #[test]
    fn selecting_preset_does_not_touch_stored_flags() {
        // Switching to a preset is a view; the custom slot (flags) stays as-is.
        let mut s = Settings { agc: true, ..Default::default() }; // custom = AGC only
        s.profile = VoiceProfile::Speaker;
        // No demote called (no edit) -> flags unchanged.
        assert!(s.agc);
        assert!(!s.echo_cancellation);
    }
}
