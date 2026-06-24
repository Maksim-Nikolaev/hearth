#!/usr/bin/env python3
"""
audio_delay.py - Measure the delay between the "mic" and "desktop" audio tracks
of OBS-style .mkv recordings.

Layout assumption (matches the recordings these were built for):
  * audio stream index 5 = mic     (recorded "instant")
  * audio stream index 6 = desktop  (the delayed copy)
Both tracks contain essentially the same signal, time-shifted; the desktop
copy lags the mic. The delay is recovered by cross-correlation, cross-checked
against per-transient onset spacing.

Usage:
    python audio_delay.py "clip1.mkv" "clip2.mkv" ...
    python audio_delay.py --mic 5 --desk 6 "clip.mkv"     # override stream indices
    python audio_delay.py --max-delay 800 "clip.mkv"      # widen lag search (ms)

Requires: ffmpeg + ffprobe on PATH, and Python packages numpy + scipy.
"""

import argparse
import subprocess
import sys
import tempfile
import os

try:
    import numpy as np
    import scipy.io.wavfile as wavfile
    import scipy.signal as sig
except ImportError:
    sys.exit("Missing dependency. Install with:  python -m pip install numpy scipy")

SR = 48000  # working sample rate


def run(cmd):
    return subprocess.run(cmd, capture_output=True, text=True)


def probe_duration(path):
    r = run(["ffprobe", "-v", "error", "-show_entries", "format=duration",
             "-of", "default=nk=1:nw=1", path])
    try:
        return float(r.stdout.strip())
    except ValueError:
        return None


def extract(path, stream, out):
    r = run(["ffmpeg", "-hide_banner", "-loglevel", "error", "-y",
             "-i", path, "-map", f"0:{stream}", "-ac", "1", "-ar", str(SR),
             "-c:a", "pcm_s16le", out])
    if r.returncode != 0 or not os.path.exists(out):
        raise RuntimeError(f"ffmpeg failed for stream {stream} of {path}:\n{r.stderr.strip()}")
    return wavfile.read(out)[1].astype(np.float64)


def envelope(x):
    """Amplitude envelope: Hilbert magnitude, low-passed. Robust when the two
    tracks differ in timbre or have several self-similar transients."""
    e = np.abs(sig.hilbert(x))
    b, a = sig.butter(4, 40 / (SR / 2))
    return sig.filtfilt(b, a, e)


def xcorr_delay(a, b, lo_ms, hi_ms):
    """Lag (ms) that best aligns b to a within [lo, hi], with parabolic
    sub-sample refinement and a normalized correlation score in [-1, 1]."""
    a = a - a.mean()
    b = b - b.mean()
    c = sig.correlate(b, a, mode="full", method="fft")
    lags = sig.correlation_lags(len(b), len(a), mode="full")
    m = (lags > int(lo_ms / 1000 * SR)) & (lags < int(hi_ms / 1000 * SR))
    cw, lw = c[m], lags[m]
    if len(cw) < 3:
        return float("nan"), 0.0
    i = int(np.argmax(cw))
    lag = lw[i]
    if 0 < i < len(cw) - 1:
        y0, y1, y2 = cw[i - 1], cw[i], cw[i + 1]
        denom = (y0 - 2 * y1 + y2)
        if denom != 0:
            lag = lw[i] + 0.5 * (y0 - y2) / denom
    norm = np.sqrt(np.sum((a) ** 2) * np.sum((b) ** 2))
    score = cw[i] / norm if norm else 0.0
    return lag / SR * 1000, float(score)


def onsets(x, frac=0.25):
    e = envelope(x)
    above = e > frac * e.max()
    return np.where((~above[:-1]) & (above[1:]))[0] / SR * 1000


def onset_diffs(mic, desk, lo_ms, hi_ms):
    mo, do = onsets(mic), onsets(desk)
    diffs = []
    for m in mo:
        cand = [d - m for d in do if lo_ms < (d - m) < hi_ms]
        if cand:
            diffs.append(min(cand))
    return diffs


def analyze(path, mic_stream, desk_stream, lo_ms, hi_ms, tmp):
    name = os.path.basename(path)
    dur = probe_duration(path)
    mic = extract(path, mic_stream, os.path.join(tmp, "mic.wav"))
    desk = extract(path, desk_stream, os.path.join(tmp, "desk.wav"))

    raw_d, raw_c = xcorr_delay(mic, desk, lo_ms, hi_ms)
    env_d, env_c = xcorr_delay(envelope(mic), envelope(desk), lo_ms, hi_ms)
    diffs = onset_diffs(mic, desk, lo_ms, hi_ms)

    # Envelope correlation is the primary estimate (reliable for both clean
    # single-transient and self-similar multi-transient clips).
    best = env_d if env_c >= 0.5 else raw_d

    print(f"\n=== {name} ===")
    if dur:
        print(f"  duration         : {dur:.2f} s")
    print(f"  raw  cross-corr  : {raw_d:7.2f} ms   (corr {raw_c:.3f})")
    print(f"  env  cross-corr  : {env_d:7.2f} ms   (corr {env_c:.3f})")
    if diffs:
        ds = ", ".join(f"{d:.1f}" for d in diffs)
        print(f"  onset spacing    : {np.mean(diffs):7.2f} ms   (per transient: {ds})")
    else:
        print(f"  onset spacing    :     n/a   (no clean transients matched)")
    conf = "high" if max(raw_c, env_c) > 0.9 else ("medium" if max(raw_c, env_c) > 0.6 else "LOW - check manually")
    print(f"  >> DELAY         : {best:7.2f} ms   [{conf}]")
    return name, best, max(raw_c, env_c)


def main():
    ap = argparse.ArgumentParser(description="Measure mic->desktop audio delay in OBS-style mkv files.")
    ap.add_argument("files", nargs="+", help="one or more .mkv files")
    ap.add_argument("--mic", type=int, default=5, help="mic audio stream index (default 5)")
    ap.add_argument("--desk", type=int, default=6, help="desktop audio stream index (default 6)")
    ap.add_argument("--min-delay", type=float, default=-50, help="min lag to search, ms (default -50)")
    ap.add_argument("--max-delay", type=float, default=700, help="max lag to search, ms (default 700)")
    args = ap.parse_args()

    results = []
    with tempfile.TemporaryDirectory() as tmp:
        for f in args.files:
            if not os.path.exists(f):
                print(f"\n=== {f} ===\n  ERROR: file not found")
                continue
            try:
                results.append(analyze(f, args.mic, args.desk,
                                       args.min_delay, args.max_delay, tmp))
            except Exception as e:
                print(f"\n=== {os.path.basename(f)} ===\n  ERROR: {e}")

    if len(results) > 1:
        print("\n" + "=" * 60)
        print("SUMMARY")
        print(f"  {'file':<42} {'delay':>9}  conf")
        for name, delay, corr in results:
            short = name if len(name) <= 40 else name[:37] + "..."
            print(f"  {short:<42} {delay:7.2f}ms  {corr:.3f}")

    # Keep the window open when double-clicked / drag-and-dropped.
    if sys.stdout.isatty() and os.name == "nt":
        try:
            input("\nDone. Press Enter to close...")
        except EOFError:
            pass


if __name__ == "__main__":
    main()
