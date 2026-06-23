<#
.SYNOPSIS
  Assemble a self-contained Windows build of the Hearth desktop app.

.DESCRIPTION
  Produces dist\hearth\ containing hearth.exe plus every GTK4 + GStreamer DLL,
  all GStreamer plugins, the gstgtk4 (gtk4paintablesink) plugin, and the GTK
  runtime assets (pixbuf loaders, GSettings schemas, icons). It runs with a
  clean PATH — nothing needs to be installed on the target machine.

  The one subtlety this handles: GTK ships a newer GLib than the GStreamer
  binaries, and gstgtk4 is built against GTK's. So GTK's DLLs are laid down
  first and GStreamer DLLs are copied only where they don't collide — the whole
  bundle then shares GTK's single, newer GLib (GStreamer is forward-compatible).

  Prerequisites: GTK4 + GStreamer installed, and gstgtk4.dll built
  (scripts\dev\build-gtk4-plugin.ps1). Run win-env.ps1 first.

.PARAMETER GtkRoot   GTK4 prefix. Default the gvsbuild location.
.PARAMETER OutDir    Output folder. Default dist\hearth.
.PARAMETER SkipBuild Use the existing release binary.
#>
param(
    [string]$GtkRoot = $(if ($env:HEARTH_GTK_ROOT) { $env:HEARTH_GTK_ROOT } else { 'C:\gtk-build\gtk\x64\release' }),
    [string]$OutDir = "$PSScriptRoot\..\..\dist\hearth",
    [switch]$SkipBuild
)

$ErrorActionPreference = 'Stop'
$repo = Resolve-Path "$PSScriptRoot\..\.."
$gst = [Environment]::GetEnvironmentVariable('GSTREAMER_1_0_ROOT_MSVC_X86_64', 'Machine')
if (-not $gst) { throw 'GSTREAMER_1_0_ROOT_MSVC_X86_64 not set.' }
$gst = $gst.TrimEnd('\')

$gstgtk4 = "$env:LOCALAPPDATA\hearth\gst-plugins\gstgtk4.dll"
if (-not (Test-Path $gstgtk4)) {
    $built = Join-Path $env:LOCALAPPDATA 'hearth\build\gst-plugins-rs\target\release\gstgtk4.dll'
    if (Test-Path $built) { $gstgtk4 = $built } else { throw "gstgtk4.dll not found; run scripts\dev\build-gtk4-plugin.ps1 first." }
}

# 1. Build release.
if (-not $SkipBuild) {
    Write-Host "Building desktop (release)..." -ForegroundColor Cyan
    Push-Location $repo
    try { cargo build -p desktop --release } finally { Pop-Location }
}
$exe = Join-Path $repo 'target\release\desktop.exe'
if (-not (Test-Path $exe)) { throw "release binary missing: $exe" }

# 2. Fresh layout.
if (Test-Path $OutDir) { Remove-Item $OutDir -Recurse -Force }
$pluginDir = Join-Path $OutDir 'lib\gstreamer-1.0'
$loaderDir = Join-Path $OutDir 'lib\gdk-pixbuf-2.0\2.10.0\loaders'
$schemaDir = Join-Path $OutDir 'share\glib-2.0\schemas'
New-Item -ItemType Directory -Force $OutDir, $pluginDir, $loaderDir, $schemaDir | Out-Null

# 3. GTK DLLs first (provides the single, newer GLib + the GTK stack).
Write-Host "Copying GTK DLLs..." -ForegroundColor Cyan
Copy-Item "$GtkRoot\bin\*.dll" $OutDir

# 4. GStreamer core DLLs, but only where GTK didn't already provide one.
Write-Host "Copying GStreamer DLLs (skipping GTK collisions)..." -ForegroundColor Cyan
Get-ChildItem "$gst\bin\*.dll" | Where-Object { -not (Test-Path (Join-Path $OutDir $_.Name)) } | Copy-Item -Destination $OutDir

# 5. All GStreamer plugins + the gtk4 paintable sink. Skip plugins whose extra
#    runtime (e.g. Python) we do not ship — they only emit load warnings.
Write-Host "Copying GStreamer plugins..." -ForegroundColor Cyan
Get-ChildItem "$gst\lib\gstreamer-1.0\*.dll" |
    Where-Object { $_.Name -notmatch 'gstpython' } |
    Copy-Item -Destination $pluginDir
Copy-Item $gstgtk4 $pluginDir -Force

# 6. gdk-pixbuf loaders + a relative cache (so the folder is relocatable).
#    Cache entries use paths relative to the cache file, resolved by gdk-pixbuf
#    against the cache's own directory.
Write-Host "Copying pixbuf loaders + regenerating cache..." -ForegroundColor Cyan
Copy-Item "$GtkRoot\lib\gdk-pixbuf-2.0\2.10.0\loaders\*.dll" $loaderDir
Push-Location (Split-Path $loaderDir -Parent)
try {
    # Force an array so a single loader isn't splatted into characters.
    $rel = @(Get-ChildItem 'loaders\*.dll' | ForEach-Object { Join-Path 'loaders' $_.Name })
    if ($rel.Count -gt 0) {
        & "$GtkRoot\bin\gdk-pixbuf-query-loaders.exe" $rel 2>$null | Set-Content -Encoding ascii 'loaders.cache'
        # query-loaders writes absolute paths; rewrite them relative to the cache
        # dir (gdk-pixbuf resolves relative entries there) so the folder relocates.
        $prefix = ((Get-Location).Path -replace '\\', '/') + '/'
        (Get-Content 'loaders.cache') -replace [regex]::Escape($prefix), '' | Set-Content -Encoding ascii 'loaders.cache'
    }
} finally { Pop-Location }

# 7. GSettings schemas (compiled).
Copy-Item "$GtkRoot\share\glib-2.0\schemas\gschemas.compiled" $schemaDir

# 8. Icon themes used by GTK (Adwaita + hicolor fallback).
foreach ($theme in 'Adwaita', 'hicolor') {
    $src = "$GtkRoot\share\icons\$theme"
    if (Test-Path $src) { Copy-Item $src (Join-Path $OutDir "share\icons\$theme") -Recurse }
}

# 9. The executable.
Copy-Item $exe (Join-Path $OutDir 'hearth.exe')

$size = [math]::Round((Get-ChildItem $OutDir -Recurse | Measure-Object Length -Sum).Sum / 1MB)
Write-Host "Packaged -> $OutDir  ($size MB)" -ForegroundColor Green
Write-Host "Test it from a clean shell:  & '$OutDir\hearth.exe'"
