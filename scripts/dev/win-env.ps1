<#
.SYNOPSIS
  Set up the shell environment to build and run Hearth on Windows (MSVC).

.DESCRIPTION
  Dot-source this once per PowerShell session, then cargo build/run/test "just
  work" without a per-command env preamble:

      . .\scripts\dev\win-env.ps1
      cargo build --workspace
      cargo run -p desktop

  It prepends GTK4, GStreamer, and cargo to PATH (so the built .exe finds its
  DLLs at runtime) and sets PKG_CONFIG_PATH / LIB so the *-sys build scripts
  link. GStreamer's location comes from the machine env var the installer sets;
  GTK's defaults to the gvsbuild prefix and is overridable via -GtkRoot or
  $env:HEARTH_GTK_ROOT.

.PARAMETER GtkRoot
  GTK4 install prefix (the dir holding bin\, lib\pkgconfig\). Default:
  C:\gtk-build\gtk\x64\release
#>
param(
    [string]$GtkRoot = $(if ($env:HEARTH_GTK_ROOT) { $env:HEARTH_GTK_ROOT } else { 'C:\gtk-build\gtk\x64\release' })
)

$ErrorActionPreference = 'Stop'

$gstRoot = [Environment]::GetEnvironmentVariable('GSTREAMER_1_0_ROOT_MSVC_X86_64', 'Machine')
if (-not $gstRoot) { throw 'GSTREAMER_1_0_ROOT_MSVC_X86_64 is not set — install the GStreamer MSVC runtime+devel first (see engine/docs/windows-setup.md).' }
$gstRoot = $gstRoot.TrimEnd('\')

if (-not (Test-Path "$GtkRoot\lib\pkgconfig\gtk4.pc")) {
    throw "GTK4 not found at '$GtkRoot' (no lib\pkgconfig\gtk4.pc). Extract the gvsbuild GTK4 bundle there or pass -GtkRoot. See engine/docs/windows-setup.md."
}

$cargoBin = "$env:USERPROFILE\.cargo\bin"

$env:Path = "$cargoBin;$GtkRoot\bin;$gstRoot\bin;$env:Path"
$env:PKG_CONFIG_PATH = "$GtkRoot\lib\pkgconfig;$gstRoot\lib\pkgconfig"
$env:LIB = "$GtkRoot\lib;$env:LIB"

Write-Host "Hearth Windows dev env ready:" -ForegroundColor Green
Write-Host "  GTK4       $GtkRoot  (gtk4 $(& "$gstRoot\bin\pkg-config.exe" --modversion gtk4 2>$null))"
Write-Host "  GStreamer  $gstRoot  ($(& "$gstRoot\bin\pkg-config.exe" --modversion gstreamer-1.0 2>$null))"
Write-Host ""
Write-Host "Next:" -ForegroundColor Cyan
Write-Host "  cargo build --workspace        # build everything"
Write-Host "  cargo run -p desktop           # launch the GUI"
Write-Host "  cargo test -p engine --lib     # engine unit tests"
Write-Host "  .\target\debug\engine.exe probe  # capture + encoder probe"
