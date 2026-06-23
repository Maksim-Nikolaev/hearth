//! Windows per-application audio enumeration via the WASAPI Audio Session API.
//!
//! Lists the processes that currently have an audio session on the default
//! render endpoint — i.e. the apps actually producing sound (Discord, a
//! browser, a game) — so the screenshare picker can offer per-app capture.
//! Each returned `AudioNode.node` is the process id as a string; the capture
//! pipeline feeds it to `wasapi2src loopback-target-pid=`.

use std::collections::HashSet;

use windows::core::Interface;
use windows::Win32::Foundation::{CloseHandle, MAX_PATH, S_OK};
use windows::Win32::Media::Audio::{
    eConsole, eRender, IAudioSessionControl2, IAudioSessionManager2, IMMDeviceEnumerator,
    MMDeviceEnumerator,
};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CLSCTX_ALL, COINIT_APARTMENTTHREADED,
};
use windows::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
};

use super::audio::AudioNode;

/// Enumerate processes with an active audio session, excluding our own process
/// and the system-sounds session. Returns an empty list on any COM failure.
pub fn list_audio_sessions() -> Vec<AudioNode> {
    unsafe { enumerate() }.unwrap_or_default()
}

unsafe fn enumerate() -> windows::core::Result<Vec<AudioNode>> {
    // GTK already initialises COM (STA) on the UI thread; a second call returns
    // S_FALSE, which is not an error. Deliberately don't CoUninitialize — GTK
    // owns the apartment lifetime.
    let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);

    let enumerator: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
    let device = enumerator.GetDefaultAudioEndpoint(eRender, eConsole)?;
    let manager: IAudioSessionManager2 = device.Activate(CLSCTX_ALL, None)?;
    let sessions = manager.GetSessionEnumerator()?;
    let count = sessions.GetCount()?;

    let own = std::process::id();
    let mut seen = HashSet::new();
    let mut out = Vec::new();

    for i in 0..count {
        let Ok(control) = sessions.GetSession(i) else { continue };
        let Ok(control2) = control.cast::<IAudioSessionControl2>() else { continue };

        // Skip the dedicated system-sounds session (returns S_OK when it is one).
        if control2.IsSystemSoundsSession() == S_OK {
            continue;
        }

        let Ok(pid) = control2.GetProcessId() else { continue };
        if pid == 0 || pid == own || !seen.insert(pid) {
            continue;
        }

        let label = process_label(pid).unwrap_or_else(|| format!("PID {pid}"));
        out.push(AudioNode { node: pid.to_string(), label });
    }

    Ok(out)
}

/// Human-friendly app name from a pid: the executable file stem, title-cased
/// (e.g. `chrome.exe` -> `Chrome`).
unsafe fn process_label(pid: u32) -> Option<String> {
    let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;

    let mut buf = [0u16; MAX_PATH as usize];
    let mut size = buf.len() as u32;
    let ok = QueryFullProcessImageNameW(
        handle,
        PROCESS_NAME_WIN32,
        windows::core::PWSTR(buf.as_mut_ptr()),
        &mut size,
    );
    let _ = CloseHandle(handle);
    ok.ok()?;

    let path = String::from_utf16_lossy(&buf[..size as usize]);
    let file = path.rsplit(['\\', '/']).next().unwrap_or(&path);
    let stem = file
        .strip_suffix(".exe")
        .or_else(|| file.strip_suffix(".EXE"))
        .unwrap_or(file);

    if stem.is_empty() {
        return None;
    }

    // Capitalise the first letter for a friendlier label.
    let mut chars = stem.chars();
    let first = chars.next()?;
    Some(first.to_uppercase().collect::<String>() + chars.as_str())
}
