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

// ── Settings ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Settings {
    pub input_device: Option<String>,
    pub output_device: Option<String>,
    pub input_volume: f64,
    pub output_volume: f64,
    pub noise_suppression: NsLevel,
    pub echo_cancellation: bool,
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
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            input_device: None,
            output_device: None,
            input_volume: 1.0,
            output_volume: 1.0,
            noise_suppression: NsLevel::Moderate,
            echo_cancellation: true,
            agc: true,
            vad: true,
            input_sensitivity: -40.0,
            activation: ActivationKind::Voice,
            ptt_key: None,
            share_width: 1920,
            share_height: 1080,
            share_fps: 30,
            share_content: ContentKind::Smoothness,
            share_audio: ShareAudioKind::None,
        }
    }
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
        assert!(matches!(s.activation, ActivationKind::Voice));
    }
}
