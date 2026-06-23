# Hearth on Windows 11

Build the `engine` crate and the full GTK4 desktop app on the Windows 11 / AMD
box. Runtime behaviour still adapts through environment variables (capture chain,
encoder, server), but the crates now need small **compile-time platform gates**:
since M7 the engine pulls in X11 (`x11rb`) and the bundled `webrtc-audio-processing`
build (autotools), neither of which works on Windows/MSVC. The `windows-dev`
branch gates both behind `cfg(target_os = "linux")` and falls back to a screen
capture with no window-enumeration and a passthrough voice DSP. See the
**Desktop app (GTK4)** section below for the GUI build.

## 1. Toolchain

1. Install Rust with the **MSVC** toolchain (`rustup default stable-x86_64-pc-windows-msvc`).
   The GStreamer MSVC libraries only link against the MSVC ABI, not GNU.
2. Install Visual Studio Build Tools (C++ workload) so `link.exe` is available.

## 2. GStreamer (MSVC, 1.24+)

Install **both** packages from <https://gstreamer.freedesktop.org/download/> for
the **MSVC 64-bit** target, "Complete" profile (includes `d3d11`, `webrtc`,
`nice`, `srtp`):

- `gstreamer-1.0-msvc-x86_64-<ver>.msi` (runtime)
- `gstreamer-1.0-devel-msvc-x86_64-<ver>.msi` (development)

Then set, e.g. in PowerShell (adjust the version path):

```powershell
$env:GSTREAMER_1_0_ROOT_MSVC_X86_64 = "C:\gstreamer\1.0\msvc_x86_64\"
$env:PATH = "$env:GSTREAMER_1_0_ROOT_MSVC_X86_64\bin;$env:PATH"
$env:PKG_CONFIG_PATH = "$env:GSTREAMER_1_0_ROOT_MSVC_X86_64\lib\pkgconfig"
```

`pkg-config` ships with the GStreamer devel package; confirm `pkg-config --modversion gstreamer-1.0` prints 1.24+.

## 3. Build

```powershell
cd engine
cargo build
```

## 4. Probe (verify capture + AMF encode)

```powershell
.\target\debug\engine.exe probe
```

**Expected:** `amfh265enc` selected (AMD AMF HEVC) and the capture chain shows the
`d3d11screencapturesrc` variant.

**If element names differ** in the installed build (the most likely snag): do
**not** recompile — override the capture chain via env. List candidates with
`gst-inspect-1.0.exe | findstr capture` and `... | findstr d3d11`, then e.g.:

```powershell
$env:HEARTH_CAPTURE = "d3d11screencapturesrc ! d3d11download ! videoconvert"
# or a DXGI/desktop-dup fallback if d3d11screencapturesrc is absent:
# $env:HEARTH_CAPTURE = "dx9screencapsrc ! videoconvert"
```

Record whatever string actually works in the README verification log.

## 5. Cross-machine call

Point both engines at one Hearth backend (run it on the Linux box; the Windows
box reaches it over the LAN). On each machine set the auth/server env, then run
`view` on one and `share` on the other. Test **both** directions.

```powershell
$env:HEARTH_HTTP = "http://<linux-lan-ip>:8080"
$env:HEARTH_WS   = "ws://<linux-lan-ip>:8080"
$env:HEARTH_USER = "alice"     # or bob on the other box
$env:HEARTH_PASS = "pw-alice"
$env:HEARTH_ROOM = "main"
# optional knobs:
$env:HEARTH_FPS = "60"
$env:HEARTH_WIDTH = "1920"; $env:HEARTH_HEIGHT = "1080"   # pin 1080p for the legibility test
$env:HEARTH_BITRATE_KBPS = "8000"
# if direct ICE fails, fall back to relay (see coturn note below):
# $env:HEARTH_TURN = "turn://user:pass@<turn-host>:3478"

.\target\debug\engine.exe share   # one box
.\target\debug\engine.exe view    # the other box
```

**Success:** both print `connection-state: Connected`; the viewer prints
`incoming stream linked -> displaying` and shows the shared screen.

Allow `engine.exe` and port 8080 through Windows Defender Firewall on the LAN.

## 6. Measurements to record (in `../README.md`)

- **Glass-to-glass latency** (target < ~150 ms LAN). Use the bench source on the
  sharer so a phone-camera stopwatch reads the same clock on both screens:
  `$env:HEARTH_CAPTURE = "videotestsrc is-live=true ! timeoverlay ! videoconvert"`.
- **1080p/60 legibility** under motion (small-text readability, smearing) — pin
  `HEARTH_WIDTH/HEIGHT=1920/1080`, `HEARTH_FPS=60`.
- **Steady-state bitrate, CPU%, GPU encoder load** on both ends (Task Manager →
  Performance → GPU → Video Encode; `radeontop` on Linux).
- **Direct ICE vs TURN**: does it connect across the two real networks without
  `HEARTH_TURN`? If not, set it and note that coturn is required for M6.

## 7. Go / No-Go

GO confirms Approach A (3-track BUNDLE over one `webrtcbin`) end-to-end on the
real Windows↔Linux mix. Otherwise escalate the screen flow to the Stage-2
dedicated transport (spec §4).

---

### coturn note (only if direct ICE fails)

A throwaway relay for the test, on the Linux box:

```bash
docker run -d --net=host coturn/coturn \
  -n --no-tls --no-dtls --fingerprint \
  --user test:test --realm hearth --listening-port 3478
# then HEARTH_TURN="turn://test:test@<linux-lan-ip>:3478" on both engines
```

---

## Desktop app (GTK4)

The `desktop` crate is a pure-Rust GTK4 + relm4 app. On Windows the cleanest
MSVC path is the **prebuilt gvsbuild GTK4 bundle** (no from-source build).

### 1. GTK4 (gvsbuild prebuilt)

Download `GTK4_Gvsbuild_<ver>_x64.zip` from
<https://github.com/wingtk/gvsbuild/releases/latest> and extract it so its files
land at the prefix baked into the bundle's `.pc` files (currently
`C:\gtk-build\gtk\x64\release`) — extracting elsewhere breaks pkg-config's
`-I`/`-L` paths. Verify:

```powershell
$gtk = "C:\gtk-build\gtk\x64\release"
$env:PKG_CONFIG_PATH = "$gtk\lib\pkgconfig;$env:GSTREAMER_1_0_ROOT_MSVC_X86_64\lib\pkgconfig"
pkg-config --modversion gtk4   # expect 4.2x
```

### 2. Build + run

GTK DLLs and GStreamer DLLs must both be on `PATH` at build and run time:

```powershell
$gtk = "C:\gtk-build\gtk\x64\release"
$gst = $env:GSTREAMER_1_0_ROOT_MSVC_X86_64.TrimEnd('\')
$env:Path = "$gtk\bin;$gst\bin;$env:USERPROFILE\.cargo\bin;$env:Path"
$env:PKG_CONFIG_PATH = "$gtk\lib\pkgconfig;$gst\lib\pkgconfig"
$env:LIB = "$gtk\lib;$env:LIB"

cargo build -p desktop      # or: cargo build --workspace
cargo run   -p desktop
```

The gvsbuild bundle ships the required runtime assets (`gschemas.compiled`,
gdk-pixbuf `loaders.cache`, Adwaita icons), so the window opens without extra
setup.

### In-window video (gtk4paintablesink)

`gtk4paintablesink` (from `gst-plugin-gtk4`) renders a remote screenshare
*inside* the window. It is **not** in the stock GStreamer MSVC build (it needs
GTK at build time), so build it once against the installed GTK4 + GStreamer:

```powershell
. .\scripts\dev\win-env.ps1
.\scripts\dev\build-gtk4-plugin.ps1     # clones gst-plugins-rs, builds + installs gstgtk4.dll
```

This installs `gstgtk4.dll` to `%LOCALAPPDATA%\hearth\gst-plugins`, which the app
adds to `GST_PLUGIN_PATH` automatically.

Two Windows-specific hazards the app handles in `main.rs`, both from GTK shipping
a **newer GLib** than the GStreamer binaries (2.88 vs 2.80):

- **PATH order** — GTK's `bin` must precede GStreamer's so GTK's GLib loads
  first (GStreamer 1.26 runs fine against the newer GLib; the reverse fails with
  "specified procedure could not be found"). `win-env.ps1` / `launch-test.ps1`
  do this; a packaged build ships a single GLib.
- **In-process scan** — GStreamer's `gst-plugin-scanner.exe` lives in the
  GStreamer install and loads the *old* GLib, so it blacklists the plugin. The
  app sets `GST_REGISTRY_FORK=no` (scan in-process) and a Hearth-owned
  `GST_REGISTRY` (isolated from the poisonable shared default).

### Push-to-talk

Global push-to-talk on Windows uses a Win32 `WH_KEYBOARD_LL` low-level keyboard
hook (engine `hotkey.rs`), so PTT fires even when the app is unfocused.

