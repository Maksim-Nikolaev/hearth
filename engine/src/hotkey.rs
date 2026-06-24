use anyhow::Result;

/// Map a human-readable X11 key name to its keysym value.
///
/// Covers the keys commonly chosen for push-to-talk. Returns `None` for
/// anything not in the table.
pub fn keysym_from_name(name: &str) -> Option<u32> {
    // Single ASCII letter/digit: GDK's key name is the character itself, and its
    // keysym is the ASCII codepoint (e.g. "g" -> 0x67, "A" -> 0x41, "5" -> 0x35).
    if name.len() == 1 {
        let c = name.as_bytes()[0];
        if c.is_ascii_alphanumeric() {
            return Some(c as u32);
        }
    }
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

#[cfg(target_os = "linux")]
pub use linux::PttGrab;
#[cfg(target_os = "windows")]
pub use win::PttGrab;
#[cfg(not(any(target_os = "linux", target_os = "windows")))]
pub use stub::PttGrab;

/// Map an X11 keysym (as returned by [`keysym_from_name`]) to the Windows
/// virtual-key code the low-level keyboard hook reports. Covers the same PTT
/// keys; `None` for anything unmapped.
#[cfg(target_os = "windows")]
fn vk_from_keysym(keysym: u32) -> Option<u32> {
    Some(match keysym {
        0x0020 => 0x20,                              // space -> VK_SPACE
        0xFFBE..=0xFFC9 => 0x70 + (keysym - 0xFFBE), // F1..F12 -> VK_F1..VK_F12
        0xFFE3 => 0xA2,                              // Control_L -> VK_LCONTROL
        0xFFE4 => 0xA3,                              // Control_R -> VK_RCONTROL
        0xFFE9 => 0xA4,                              // Alt_L -> VK_LMENU
        0xFFEA => 0xA5,                              // Alt_R -> VK_RMENU
        0xFFE1 => 0xA0,                              // Shift_L -> VK_LSHIFT
        0xFFE2 => 0xA1,                              // Shift_R -> VK_RSHIFT
        0x0041..=0x005A => keysym,                   // A-Z -> VK_A..VK_Z
        0x0061..=0x007A => keysym - 0x20,            // a-z -> VK_A..VK_Z (uppercase)
        0x0030..=0x0039 => keysym,                   // 0-9 -> VK_0..VK_9
        _ => return None,
    })
}

/// X11 global push-to-talk grab. Linux/X11 only.
#[cfg(target_os = "linux")]
mod linux {
    use super::Result;
    use anyhow::anyhow;
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    };
    use std::thread::JoinHandle;
    use x11rb::connection::Connection;
    use x11rb::protocol::xproto::{ConnectionExt, GrabMode, ModMask};
    use x11rb::protocol::Event;

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
} // mod linux

/// Windows global push-to-talk via a `WH_KEYBOARD_LL` low-level keyboard hook.
///
/// The hook is installed on a dedicated thread that runs a message loop (a
/// low-level hook's callback only fires while its installing thread pumps
/// messages). The callback runs on that thread, so the target key + closure
/// live in a `thread_local`; `Drop` posts `WM_QUIT` to unwind the loop and
/// joins the thread.
#[cfg(target_os = "windows")]
mod win {
    use super::Result;
    use anyhow::anyhow;
    use std::cell::RefCell;
    use std::sync::mpsc;
    use std::thread::JoinHandle;
    use windows_sys::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows_sys::Win32::System::Threading::GetCurrentThreadId;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CallNextHookEx, DispatchMessageW, GetMessageW, PostThreadMessageW, SetWindowsHookExW,
        TranslateMessage, UnhookWindowsHookEx, HC_ACTION, HHOOK, KBDLLHOOKSTRUCT, MSG,
        WH_KEYBOARD_LL, WM_KEYDOWN, WM_KEYUP, WM_QUIT, WM_SYSKEYDOWN, WM_SYSKEYUP,
    };

    struct HookState {
        vk: u32,
        held: bool,
        on_change: Box<dyn Fn(bool) + Send>,
    }

    thread_local! {
        // Set on the hook thread before the hook is installed; the callback
        // runs on that same thread, so a thread_local is sound and lock-free.
        static STATE: RefCell<Option<HookState>> = const { RefCell::new(None) };
    }

    unsafe extern "system" fn hook_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        if code == HC_ACTION as i32 {
            let kb = &*(lparam as *const KBDLLHOOKSTRUCT);
            STATE.with(|s| {
                if let Some(st) = s.borrow_mut().as_mut() {
                    if kb.vkCode == st.vk {
                        let msg = wparam as u32;
                        if msg == WM_KEYDOWN || msg == WM_SYSKEYDOWN {
                            if !st.held {
                                st.held = true;
                                (st.on_change)(true);
                            }
                        } else if (msg == WM_KEYUP || msg == WM_SYSKEYUP) && st.held {
                            st.held = false;
                            (st.on_change)(false);
                        }
                    }
                }
            });
        }

        CallNextHookEx(0 as HHOOK, code, wparam, lparam)
    }

    pub struct PttGrab {
        thread_id: u32,
        handle: Option<JoinHandle<()>>,
    }

    impl PttGrab {
        pub fn grab(keysym: u32, on_change: impl Fn(bool) + Send + 'static) -> Result<Self> {
            let vk = super::vk_from_keysym(keysym)
                .ok_or_else(|| anyhow!("no Windows virtual-key mapping for keysym 0x{keysym:04X}"))?;

            // The thread reports back its id (for WM_QUIT) or a setup error.
            let (tx, rx) = mpsc::channel::<Result<u32>>();

            let handle = std::thread::spawn(move || {
                STATE.with(|s| {
                    *s.borrow_mut() = Some(HookState {
                        vk,
                        held: false,
                        on_change: Box::new(on_change),
                    });
                });

                let hook = unsafe {
                    SetWindowsHookExW(WH_KEYBOARD_LL, Some(hook_proc), GetModuleHandleW(std::ptr::null()), 0)
                };
                if hook == 0 as HHOOK {
                    let _ = tx.send(Err(anyhow!("SetWindowsHookExW(WH_KEYBOARD_LL) failed")));
                    STATE.with(|s| *s.borrow_mut() = None);
                    return;
                }

                let _ = tx.send(Ok(unsafe { GetCurrentThreadId() }));

                // Pump messages so the hook fires; GetMessageW returns 0 on the
                // WM_QUIT posted by Drop, -1 on error.
                let mut msg: MSG = unsafe { std::mem::zeroed() };
                while unsafe { GetMessageW(&mut msg, 0 as _, 0, 0) } > 0 {
                    unsafe {
                        TranslateMessage(&msg);
                        DispatchMessageW(&msg);
                    }
                }

                unsafe { UnhookWindowsHookEx(hook) };
                STATE.with(|s| *s.borrow_mut() = None);
            });

            match rx.recv() {
                Ok(Ok(thread_id)) => Ok(Self { thread_id, handle: Some(handle) }),
                Ok(Err(e)) => {
                    let _ = handle.join();
                    Err(e)
                },
                Err(_) => {
                    let _ = handle.join();
                    Err(anyhow!("PTT hook thread exited during setup"))
                },
            }
        }
    }

    impl Drop for PttGrab {
        fn drop(&mut self) {
            unsafe { PostThreadMessageW(self.thread_id, WM_QUIT, 0, 0) };
            if let Some(h) = self.handle.take() {
                let _ = h.join();
            }
        }
    }
}

/// Push-to-talk stub for platforms without a global-grab backend yet (macOS:
/// needs a `CGEventTap`). Keeps the engine compiling and the app running;
/// [`grab`](PttGrab::grab) reports the limitation instead of silently failing.
#[cfg(not(any(target_os = "linux", target_os = "windows")))]
mod stub {
    use super::Result;
    use anyhow::anyhow;

    pub struct PttGrab {
        _private: (),
    }

    impl PttGrab {
        pub fn grab(_keysym: u32, _on_change: impl Fn(bool) + Send + 'static) -> Result<Self> {
            Err(anyhow!(
                "global push-to-talk is not yet implemented on this platform"
            ))
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

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_maps_named_keys_to_virtual_keys() {
        assert_eq!(vk_from_keysym(keysym_from_name("space").unwrap()), Some(0x20));
        assert_eq!(vk_from_keysym(keysym_from_name("F1").unwrap()), Some(0x70));
        assert_eq!(vk_from_keysym(keysym_from_name("F12").unwrap()), Some(0x7B));
        assert_eq!(vk_from_keysym(keysym_from_name("Control_L").unwrap()), Some(0xA2));
        assert_eq!(vk_from_keysym(0xFFFF), None);
    }

    // Exercises the full FFI lifecycle: install the WH_KEYBOARD_LL hook, run the
    // message-loop thread, then Drop (PostThreadMessage(WM_QUIT) + join). Does
    // not synthesise input — key detection is covered by the live call test.
    #[cfg(target_os = "windows")]
    #[test]
    fn windows_ptt_installs_and_drops_cleanly() {
        let keysym = keysym_from_name("F12").unwrap();
        let grab = PttGrab::grab(keysym, |_held| {}).expect("install hook");
        drop(grab); // must not hang
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_ptt_rejects_unmappable_keysym() {
        assert!(PttGrab::grab(0xFFFF, |_| {}).is_err());
    }
}
