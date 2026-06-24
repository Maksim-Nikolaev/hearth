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
Use a split-track layout via *Advanced Audio Properties → Tracks*, then enable
those tracks under *Settings → Output → Recording → Audio Track*:
- **Track 1 = System + Mic** (the mix), **Track 2 = Mic only**, **Track 3 =
  System only**. Name the tracks so auto-detection can find them.
- The delay is measured between **Mic only** (your instant signal) and **System
  only** (the same voice after it traveled through Hearth and out the speakers).

Then:
1. Talk in short bursts (claps/syllables make clean transients).
2. Save as `.mkv` (robust to crashes; remux to mp4 later if needed).

### Run it
**Drag-and-drop** — drop one or more `.mkv` files onto the launcher:
- **Linux (KDE/GNOME):** `Measure audio delay.desktop` (a copy is installed on the
  Desktop). First drop creates a private venv under
  `~/.cache/hearth-audio-delay/` — no sudo, no system packages.
- **Windows:** `Measure audio delay.bat`.

From any terminal:
```sh
./audio_delay.sh "clip1.mkv" "clip2.mkv" ...   # Linux (self-bootstraps the venv)
python audio_delay.py "clip1.mkv" ...          # if numpy/scipy are already present
```

Reports three estimates per file — raw cross-correlation, **envelope
cross-correlation (the primary estimate)**, and onset spacing — plus a `>> DELAY`
line with a confidence flag.

### Stream indices (auto-detected)
By default the streams are **auto-detected from the OBS track titles**: a track
titled `Mic only` becomes the mic, `System only` (or `Desktop`) becomes the
system reference. This makes drag-and-drop work with no flags. If a recording has
untitled tracks, it falls back to the last two audio streams, then to the legacy
Windows layout (mic = stream 5, desktop = stream 6).

Override per file when needed — check the layout with `ffprobe file.mkv`:
```sh
python audio_delay.py --mic 2 --desk 3 "clip.mkv"
```
Other flags: `--max-delay 1000` widens the search window (default −50…700 ms) for
very large buffers.

Requires `ffmpeg` + `ffprobe` on `PATH`. `numpy` + `scipy` are provided by the
launcher's venv; for a bare `python audio_delay.py` run, install them yourself.

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
