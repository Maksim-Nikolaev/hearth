<#
.SYNOPSIS
  Build gtk4paintablesink (gstgtk4.dll) for Windows and install it where the
  desktop app looks for it.

.DESCRIPTION
  gst-plugin-gtk4 is the GStreamer element that renders incoming video inside
  the GTK window. It is NOT in the stock GStreamer binaries (it needs GTK at
  build time), so build it from gst-plugins-rs against the installed GTK4 +
  GStreamer, then drop the DLL in %LOCALAPPDATA%\hearth\gst-plugins (the dir
  the app adds to GST_PLUGIN_PATH).

  Prerequisites: GTK4 + GStreamer installed (engine/docs/windows-setup.md) and
  this shell set up via:  . .\scripts\dev\win-env.ps1

  Note the runtime caveats the app already handles (main.rs): GTK must lead on
  PATH (its GLib is newer than GStreamer's), and the app forces an in-process,
  Hearth-owned plugin scan so the plugin is not blacklisted by GStreamer's
  scanner subprocess.

.PARAMETER Branch    gst-plugins-rs branch to build (matches gstreamer-rs 0.23).
.PARAMETER WorkDir   Where to clone/build. Persists so rebuilds are incremental.
#>
param(
    [string]$Branch = '0.13',
    [string]$WorkDir = "$env:LOCALAPPDATA\hearth\build"
)

$ErrorActionPreference = 'Stop'

if (-not $env:PKG_CONFIG_PATH) { throw 'Run ". .\scripts\dev\win-env.ps1" first (PKG_CONFIG_PATH not set).' }

New-Item -ItemType Directory -Force $WorkDir | Out-Null
$repo = Join-Path $WorkDir 'gst-plugins-rs'

if (-not (Test-Path "$repo\.git")) {
    Write-Host "Cloning gst-plugins-rs ($Branch)..." -ForegroundColor Cyan
    # Long, deeply-nested example paths blow past MAX_PATH on checkout; enable
    # long paths and skip the net/webrtc examples we don't need.
    git -c core.longpaths=true clone --depth 1 --branch $Branch `
        https://gitlab.freedesktop.org/gstreamer/gst-plugins-rs.git $repo
    git -C $repo config core.longpaths true
    git -C $repo sparse-checkout init --no-cone
    git -C $repo sparse-checkout set "/*" "!/net/webrtc/examples"
    git -C $repo checkout
}

Write-Host "Building gst-plugin-gtk4 (release)..." -ForegroundColor Cyan
Push-Location $repo
try { cargo build --release -p gst-plugin-gtk4 } finally { Pop-Location }

$dll = Join-Path $repo 'target\release\gstgtk4.dll'
if (-not (Test-Path $dll)) { throw "build did not produce $dll" }

$dest = "$env:LOCALAPPDATA\hearth\gst-plugins"
New-Item -ItemType Directory -Force $dest | Out-Null
Copy-Item $dll $dest -Force

Write-Host "Installed gstgtk4.dll -> $dest" -ForegroundColor Green
Write-Host "In-window video will work the next time you run the desktop app."
