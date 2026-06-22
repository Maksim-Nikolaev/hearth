/// Which display surface to capture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShareSource {
    /// Capture a full monitor by index (0 = primary).
    Screen { monitor: usize },
    /// Capture a single X11 window by its XID.
    Window { xid: u32 },
}

/// Trade-off hint passed to the capture pipeline and encoder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContentType {
    /// Prioritise frame rate (motion, games). Keep the configured fps ceiling.
    Smoothness,
    /// Prioritise sharpness (text, slides). Cap fps at 15 and raise encoder
    /// quality (lower quantiser) so each frame is crisper.
    Clarity,
}

/// Full configuration for a screenshare session.
#[derive(Debug, Clone)]
pub struct ShareConfig {
    pub source: ShareSource,
    pub width: u32,
    pub height: u32,
    /// Desired frame rate. `ContentType::Clarity` silently caps this at 15.
    pub fps: u32,
    pub content: ContentType,
    /// Optional audio source to capture alongside the video track.
    pub audio: crate::screen::audio::ShareAudio,
    /// Target encoder bitrate in kbps. Overridden by `HEARTH_BITRATE_KBPS` env var.
    pub bitrate_kbps: u32,
}

impl Default for ShareConfig {
    fn default() -> Self {
        Self {
            source: ShareSource::Screen { monitor: 0 },
            width: 1920,
            height: 1080,
            fps: 30,
            content: ContentType::Smoothness,
            audio: crate::screen::audio::ShareAudio::None,
            bitrate_kbps: 6000,
        }
    }
}

impl ShareConfig {
    pub fn new() -> Self {
        Self::default()
    }
}

/// A window visible on the desktop, returned by [`list_windows`].
#[derive(Debug, Clone)]
pub struct ShareWindow {
    pub xid: u32,
    pub title: String,
}

/// Enumerate top-level X11 windows via `_NET_CLIENT_LIST`.
///
/// Returns an empty list if the display is unavailable or the WM does not
/// support the EWMH protocol. Never panics.
pub fn list_windows() -> Vec<ShareWindow> {
    list_windows_inner().unwrap_or_default()
}

fn list_windows_inner() -> Option<Vec<ShareWindow>> {
    use x11rb::connection::Connection;
    use x11rb::protocol::xproto::{AtomEnum, ConnectionExt as _};
    use x11rb::rust_connection::RustConnection;

    let (conn, screen_num) = RustConnection::connect(None).ok()?;
    let root = conn.setup().roots[screen_num].root;

    // Resolve atom names we need.
    let net_client_list = conn.intern_atom(false, b"_NET_CLIENT_LIST").ok()?.reply().ok()?.atom;
    let net_wm_name = conn.intern_atom(false, b"_NET_WM_NAME").ok()?.reply().ok()?.atom;
    let utf8_string = conn.intern_atom(false, b"UTF8_STRING").ok()?.reply().ok()?.atom;

    // Read the root window's _NET_CLIENT_LIST (list of window XIDs as WINDOW atoms).
    // Type 0 = AnyPropertyType.
    let list_reply = conn
        .get_property(false, root, net_client_list, 0u32, 0, u32::MAX)
        .ok()?
        .reply()
        .ok()?;

    // Each element is a 32-bit window XID.
    let xids: Vec<u32> = list_reply
        .value32()
        .map(|iter| iter.collect())
        .unwrap_or_default();

    let mut windows = Vec::with_capacity(xids.len());

    for xid in xids {
        let title = window_title(&conn, xid, net_wm_name, utf8_string, u32::from(AtomEnum::STRING));

        windows.push(ShareWindow { xid, title });
    }

    Some(windows)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_bitrate_is_6000() {
        assert_eq!(ShareConfig::default().bitrate_kbps, 6000);
    }
}

/// Try `_NET_WM_NAME` (UTF-8) first, fall back to `WM_NAME` (Latin-1).
fn window_title(
    conn: &x11rb::rust_connection::RustConnection,
    xid: u32,
    net_wm_name: u32,
    utf8_string: u32,
    string_atom: u32,
) -> String {
    use x11rb::protocol::xproto::ConnectionExt as _;

    let try_prop = |atom: u32, type_atom: u32| -> Option<String> {
        // Type 0 = AnyPropertyType.
        let reply = conn
            .get_property(false, xid, atom, 0u32, 0, u32::MAX)
            .ok()?
            .reply()
            .ok()?;

        if reply.type_ != type_atom || reply.value.is_empty() {
            return None;
        }

        String::from_utf8(reply.value).ok()
    };

    try_prop(net_wm_name, utf8_string)
        .or_else(|| try_prop(u32::from(x11rb::protocol::xproto::AtomEnum::WM_NAME), string_atom))
        .unwrap_or_else(|| format!("0x{xid:x}"))
}
