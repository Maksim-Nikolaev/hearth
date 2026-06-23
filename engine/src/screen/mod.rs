pub mod audio;
#[cfg(target_os = "windows")]
#[allow(dead_code)] // enumeration is ready; wired back in once capture is isolated
mod audio_win;
pub mod backend;
pub mod capture;
pub mod source;
pub mod sources;
pub mod thumbnail;

pub use audio::{AudioNode, ShareAudio, has_pipewire, list_app_nodes, screen_audio_chain};
pub use backend::{CaptureBackend, detect_capture_backend};
pub use capture::capture_chain;
pub use source::ScreenSource;
pub use sources::{ContentType, ShareConfig, ShareSource, ShareWindow, list_windows};
pub use thumbnail::thumbnail;
