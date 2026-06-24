# Run the IAudioClient3 low-latency loopback spike with the dev env set up.
# (engine.exe links GStreamer DLLs at load time, so the env must be on PATH.)
#
# Usage:  .\scripts\measure\wasapi3.ps1 [seconds]   (default 20)
#
# Talk into the mic while it runs; measure Mic vs Entire System in OBS to get
# the raw WASAPI device floor (no GStreamer / Opus / network).

param([int]$Seconds = 20)

. "$PSScriptRoot\..\dev\win-env.ps1" | Out-Null
& "$PSScriptRoot\..\..\target\debug\engine.exe" wasapi3 $Seconds
