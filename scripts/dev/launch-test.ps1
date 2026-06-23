<#
.SYNOPSIS
  Windows two-instance local test: build the desktop app, mint tokens for the
  dev users alice + bob, and launch both windows against the dev backend.

.DESCRIPTION
  Mirrors scripts/dev/launch-test.sh for Windows. The backend must be running on
  :8080 first — easiest is fully containerised:

      docker compose -f compose.dev.yml up -d

  Then:

      .\scripts\dev\launch-test.ps1               # build + launch alice & bob
      .\scripts\dev\launch-test.ps1 -SkipBuild    # launch the existing build
      .\scripts\dev\launch-test.ps1 -Synthetic    # fake screen (UI-safe)
      .\scripts\dev\launch-test.ps1 -Release      # optimised build

  Users alice/bob (pw-alice/pw-bob) must already be provisioned in the backend
  (same prerequisite as the Linux script).

.PARAMETER SkipBuild  Launch the existing binary without rebuilding.
.PARAMETER Synthetic  Feed videotestsrc to screenshare/preview instead of the real screen.
.PARAMETER Release    Build/launch the optimised release binary.
#>
param(
    [switch]$SkipBuild,
    [switch]$Synthetic,
    [switch]$Release
)

$ErrorActionPreference = 'Stop'
$root = Resolve-Path "$PSScriptRoot\..\.."
Set-Location $root

$base = if ($env:HEARTH_HTTP) { $env:HEARTH_HTTP } else { 'http://localhost:8080' }
$profile = if ($Release) { 'release' } else { 'debug' }
$bin = Join-Path $root "target\$profile\desktop.exe"

# 1. Backend reachable?
try { $null = Invoke-RestMethod "$base/health" -TimeoutSec 4 }
catch {
    Write-Host "X backend not reachable on $base" -ForegroundColor Red
    Write-Host "  start it first:  docker compose -f compose.dev.yml up -d" -ForegroundColor Yellow
    exit 1
}
Write-Host "OK backend up on $base" -ForegroundColor Green

# 2. Build (sets the GTK/GStreamer env via win-env.ps1).
. "$PSScriptRoot\win-env.ps1" | Out-Null
if ($SkipBuild) {
    if (-not (Test-Path $bin)) { throw "-SkipBuild given but $bin does not exist; run once without it." }
    Write-Host "- skipping build, using $bin"
} else {
    Write-Host "- building desktop ($profile)..."
    if ($Release) { cargo build -p desktop --release } else { cargo build -p desktop }
}

# 3. Mint fresh tokens for alice + bob.
function Get-Token($user) {
    $body = @{ username = $user; password = "pw-$user" } | ConvertTo-Json
    (Invoke-RestMethod "$base/auth/login" -Method Post -ContentType 'application/json' -Body $body).access_token
}

# 4. Stop any prior test windows.
Get-Process desktop -ErrorAction SilentlyContinue | Stop-Process -Force
Start-Sleep -Milliseconds 500

# 5. Optional synthetic capture.
if ($Synthetic) {
    $env:HEARTH_CAPTURE = 'videotestsrc is-live=true pattern=ball ! timeoverlay ! videoconvert'
    Write-Host "- synthetic capture enabled"
}

# 6. Launch both windows, each logging to the temp dir.
function Start-Instance($name) {
    $log = Join-Path $env:TEMP "hearth-$name.log"
    $env:HEARTH_TITLE = $name
    $env:HEARTH_TOKEN = Get-Token $name
    $p = Start-Process $bin -PassThru -RedirectStandardError $log -RedirectStandardOutput "$log.out"
    Write-Host "  $name -> pid $($p.Id) (log: $log)"
}

Write-Host "- launching windows..."
Start-Instance alice
Start-Instance bob
Remove-Item Env:\HEARTH_TITLE, Env:\HEARTH_TOKEN -ErrorAction SilentlyContinue

Write-Host ""
Write-Host "OK alice + bob running." -ForegroundColor Green
Write-Host "  logs:  Get-Content `"$env:TEMP\hearth-alice.log`" -Wait"
Write-Host "  stop:  Get-Process desktop | Stop-Process"
