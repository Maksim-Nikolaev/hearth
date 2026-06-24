# Latency measurement

Tools for measuring real mouth-to-ear voice latency from OBS recordings, plus a
pointer to the always-on in-app per-hop logging.

## End-to-end: `audio_delay.py`

Measures the delay between two audio tracks of an OBS-style `.mkv` recording —
**Mic** (the talker's instant signal) vs **Entire System** (the same voice after
it has traveled through Hearth and out the speakers). Cross-correlates the two,
cross-checked against per-transient onset spacing. This is the ground-truth
number (e.g. the 151 → 124 → 92 ms progression).

### How to record (OBS)
1. Two audio tracks: **Track 1 = Mic/Aux** (your real mic), **Track 2 = Desktop
   Audio** ("Entire System" — what the receiving end plays out the speakers).
2. Talk in short bursts (claps/syllables make clean transients).
3. Save as `.mkv`.

### Run it
```sh
# drag one or more .mkv files onto "Measure audio delay.bat"  (Windows)
# or from any terminal:
python audio_delay.py "clip1.mkv" "clip2.mkv" ...
```

Reports three estimates per file — raw cross-correlation, **envelope
cross-correlation (the primary estimate)**, and onset spacing — plus a `>> DELAY`
line with a confidence flag.

### Stream indices (important, platform-specific)
Defaults to **mic = stream 5, desktop = stream 6**, which is how these particular
Windows OBS recordings are laid out. **A different OBS config / Linux capture may
number the streams differently** — check with `ffprobe file.mkv` and override:
```sh
python audio_delay.py --mic 1 --desk 2 "clip.mkv"
```
Other flags: `--max-delay 1000` widens the search window (default −50…700 ms) for
very large buffers.

Requires `ffmpeg` + `ffprobe` on `PATH` and Python `numpy` + `scipy`.

## Per-hop: in-app `[latency]` logging

The engine logs the actual buffer delay at each voice hop every ~2 s (always on):
```
[latency] voice <peer>: configured live=… min=…ms   ← jitter buffer + sink (recv buffering)
[latency] voice send  (mic -> wire): X ms            ← DSP frame + Opus encode + pay
[latency] voice recv  (wire -> speaker, post-jitter): Y ms  ← decode + convert + sink
```
Watch them live:
```powershell
Get-Content "$env:TEMP\hearth-alice.log" -Wait | Select-String "latency"
```
For **element-level** detail, launch with `HEARTH_LATENCY_TRACE=1` to enable
GStreamer's built-in latency tracer.

The gap between (send + jitter + recv) and the OBS end-to-end number is the
**OS audio-engine floor** (the two WASAPI/PipeWire device crossings) — see
`docs/research/voice-transport.md`.
