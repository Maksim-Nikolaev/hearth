use anyhow::{anyhow, Result};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::thread::JoinHandle;
use x11rb::connection::Connection;
use x11rb::protocol::xproto::{ConnectionExt, GrabMode, ModMask};
use x11rb::protocol::Event;

/// Map a human-readable X11 key name to its keysym value.
///
/// Covers the keys commonly chosen for push-to-talk. Returns `None` for
/// anything not in the table.
pub fn keysym_from_name(name: &str) -> Option<u32> {
    Some(match name {
        "space" => 0x0020,
        "F1" => 0xFFBE,
        "F2" => 0xFFBF,
        "F3" => 0xFFC0,
        "F4" => 0xFFC1,
        "F5" => 0xFFC2,
        "F6" => 0xFFC3,
        "F7" => 0xFFC4,
        "F8" => 0xFFC5,
        "F9" => 0xFFC6,
        "F10" => 0xFFC7,
        "F11" => 0xFFC8,
        "F12" => 0xFFC9,
        "Control_L" => 0xFFE3,
        "Control_R" => 0xFFE4,
        "Alt_L" => 0xFFE9,
        "Alt_R" => 0xFFEA,
        "Shift_L" => 0xFFE1,
        "Shift_R" => 0xFFE2,
        _ => return None,
    })
}

/// Convert a keysym to the X11 keycode by querying the keyboard mapping.
///
/// The mapping is queried once at grab time; this is not intended to be
/// called in a hot loop.
fn keysym_to_keycode(
    conn: &impl Connection,
    keysym: u32,
) -> Result<u8> {
    let setup = conn.setup();
    let min = setup.min_keycode;
    let max = setup.max_keycode;
    let count = max - min + 1;

    let reply = conn
        .get_keyboard_mapping(min, count)?
        .reply()?;

    let per = reply.keysyms_per_keycode as usize;

    for (i, chunk) in reply.keysyms.chunks(per).enumerate() {
        if chunk.contains(&keysym) {
            return Ok(min + i as u8);
        }
    }

    Err(anyhow!("keysym 0x{keysym:04X} not found in keyboard mapping"))
}

/// An active X11 global key grab.
///
/// Created via [`PttGrab::grab`]. Holds a background thread that polls the X
/// connection for `KeyPress`/`KeyRelease` events and invokes the callback.
/// The grab is released and the thread joined on `Drop`.
pub struct PttGrab {
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl PttGrab {
    /// Grab `keysym` on the X11 root window and invoke `on_change(true)` on
    /// press and `on_change(false)` on release, even when the application does
    /// not have focus.
    ///
    /// Spawns a background thread that polls for events every 8 ms. On `Drop`
    /// the grab is released and the thread is joined.
    pub fn grab(keysym: u32, on_change: impl Fn(bool) + Send + 'static) -> Result<Self> {
        let (conn, screen_num) = x11rb::connect(None)?;

        let root = conn.setup().roots[screen_num].root;
        let keycode = keysym_to_keycode(&conn, keysym)?;

        // Grab the key for every modifier combination so PTT fires regardless
        // of Caps Lock, Num Lock, etc.
        conn.grab_key(
            true,
            root,
            ModMask::ANY,
            keycode,
            GrabMode::ASYNC,
            GrabMode::ASYNC,
        )?
        .check()?;

        conn.flush()?;

        let stop = Arc::new(AtomicBool::new(false));
        let stop_thread = stop.clone();

        let handle = std::thread::spawn(move || {
            while !stop_thread.load(Ordering::Relaxed) {
                match conn.poll_for_event() {
                    Ok(Some(Event::KeyPress(e))) if e.detail == keycode => on_change(true),
                    Ok(Some(Event::KeyRelease(e))) if e.detail == keycode => on_change(false),
                    Ok(_) => std::thread::sleep(std::time::Duration::from_millis(8)),
                    Err(_) => break,
                }
            }

            // Release the grab before the connection closes.
            let _ = conn.ungrab_key(keycode, root, ModMask::ANY);
            let _ = conn.flush();
        });

        Ok(Self { stop, handle: Some(handle) })
    }
}

impl Drop for PttGrab {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);

        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_key_names_to_keysyms() {
        assert_eq!(keysym_from_name("F12"), Some(0xFFC9));
        assert_eq!(keysym_from_name("space"), Some(0x0020));
        assert_eq!(keysym_from_name("not-a-key"), None);
    }
}
