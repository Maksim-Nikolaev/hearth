use std::path::PathBuf;

const SERVICE: &str = "hearth";
const TOKEN_USER: &str = "access-token";

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
}

fn token_file() -> Option<PathBuf> {
    directories::ProjectDirs::from("dev", "hearth", "hearth").map(|d| d.config_dir().join("token"))
}
