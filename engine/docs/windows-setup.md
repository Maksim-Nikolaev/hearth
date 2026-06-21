# Engine on Windows 11 (Task 5 runbook)

Goal: build the same `engine` crate on the Windows 11 / AMD box and run the
cross-machine screenshare test against a Hearth backend, with **no source edits**
(everything adapts through environment variables).

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
