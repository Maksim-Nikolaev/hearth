#!/usr/bin/env python3
"""
loopback_latency.py - Live mouth-to-ear latency by chirp cross-correlation.

Plays a short chirp out one audio device and records from another *on the same
machine* (one sound clock), then cross-correlates the recording against the
emitted chirp to recover the play->record delay in milliseconds. Repeats N times
and reports the median, so an adaptive jitter buffer can't fool a single shot.

Why a chirp, not a click: Opus, noise suppression and AGC mangle a bare impulse;
a swept sine survives band-limiting and gain riding and still cross-correlates to
a sharp peak.

WHY SAME-PC TWO-CLIENT IS THE BEST RIG
  One machine = one audio clock, so the outgoing chirp and its delayed return are
  timed by the same ruler (no two-clock skew). It also removes the real network,
  so you measure the app *pipeline* (capture buffer + codec + jitter buffer +
  playback buffer). Add RTT/2 for a WAN estimate.

ROUTING (Linux / PipeWire) - measures client A -> client B, one direction:
  # 1. A virtual sink whose monitor is client A's microphone:
  pactl load-module module-null-sink sink_name=A_mic sink_properties=device.description=A_mic
  # 2. A virtual sink we record, fed by client B's output:
  pactl load-module module-null-sink sink_name=B_out sink_properties=device.description=B_out
  # 3. In each client's audio settings:
  #      client A  input  = "Monitor of A_mic"      (we inject the chirp here)
  #      client A  output = some dummy / muted
  #      client B  input  = some dummy / muted
  #      client B  output = "B_out"                 (we record here)
  #    Put A and B in the same voice room.
  # 4. Run, pointing --out at A_mic and --in at the monitor of B_out:
  python loopback_latency.py --out A_mic --in B_out.monitor --trials 15
  # Turn OFF noise suppression / echo cancellation in both clients for the test -
  # AEC can treat the chirp as echo and cancel it.

ROUTING (Windows): install VB-CABLE (one or two cables). Point client A's mic at
"CABLE Output", play into "CABLE Input"; capture client B's output through a
second cable. Same idea, same flags (use --list to find device names).

CALIBRATION: --calibrate loops the chosen out straight into the chosen in with no
app in between (wire A_mic's monitor directly to B_out, or pick the same loopback
device for both) to measure the rig's own baseline; subtract it from app numbers.

Usage:
  python loopback_latency.py --list                       # enumerate devices
  python loopback_latency.py --out A_mic --in B_out.monitor --trials 15
  python loopback_latency.py --out CABLE-A --in CABLE-B --max-delay 400

Requires: Python packages numpy, scipy, sounddevice (pip install numpy scipy sounddevice).
"""

import argparse
import statistics
import sys
import time

try:
    import numpy as np
    import scipy.signal as sig
except ImportError:
    sys.exit("Missing dependency. Install with:  python -m pip install numpy scipy")

SR = 48000  # working sample rate (matches the voice path)


def _sounddevice():
    """Import sounddevice lazily so the pure DSP helpers (make_chirp, xcorr_delay)
    stay importable - and unit-testable - without PortAudio or any audio device."""
    try:
        import sounddevice as sd

        return sd
    except ImportError:
        sys.exit("Missing 'sounddevice'. Install:  apt install python3-sounddevice libportaudio2"
                 "  (or pip install sounddevice).")


def make_chirp(dur_ms, f0, f1, sr):
    """A Hann-windowed linear sweep f0->f1. The window kills edge transients so the
    correlation peak isn't smeared by the chirp's own on/off clicks."""
    n = int(dur_ms / 1000 * sr)
    t = np.arange(n) / sr
    sweep = sig.chirp(t, f0=f0, f1=f1, t1=t[-1], method="linear")

    return (sweep * np.hanning(n)).astype(np.float32)


def xcorr_delay(ref, rec, lo_ms, hi_ms, sr):
    """Lag (ms) that best aligns rec to ref within [lo, hi], with parabolic
    sub-sample refinement and a normalized correlation score in [0, 1]."""
    ref = ref - ref.mean()
    rec = rec - rec.mean()

    c = sig.correlate(rec, ref, mode="full", method="fft")
    lags = sig.correlation_lags(len(rec), len(ref), mode="full")

    m = (lags >= int(lo_ms / 1000 * sr)) & (lags <= int(hi_ms / 1000 * sr))
    cw, lw = c[m], lags[m]
    if len(cw) < 3:
        return float("nan"), 0.0

    i = int(np.argmax(cw))
    lag = float(lw[i])
    if 0 < i < len(cw) - 1:
        y0, y1, y2 = cw[i - 1], cw[i], cw[i + 1]
        denom = y0 - 2 * y1 + y2
        if denom != 0:
            lag = lw[i] + 0.5 * (y0 - y2) / denom

    norm = np.sqrt(np.sum(ref**2) * np.sum(rec**2))
    score = float(cw[i] / norm) if norm else 0.0

    return lag / sr * 1000, score


def _write_read(istream, ostream, out):
    """Play `out` while recording the same number of frames, on two *independent*
    streams: a single split-device duplex stream (sd.playrec across two devices)
    is unsupported by PortAudio's PulseAudio backend and just hangs. A reader
    thread drains the input while the main thread writes the output."""
    import threading

    nframes = len(out)
    rec = np.zeros((nframes, 1), dtype=np.float32)

    def reader():
        i = 0
        while i < nframes:
            block, _ = istream.read(min(2048, nframes - i))
            rec[i:i + len(block)] = block
            i += len(block)

    t = threading.Thread(target=reader, daemon=True)
    t.start()
    ostream.write(out)
    t.join()

    return rec[:, 0]


def _segment(chirp, max_delay_ms, sr):
    """One measurement burst: the chirp followed by enough silence for its delayed
    return to land inside the capture window."""
    pad = np.zeros(int((max_delay_ms + 50) / 1000 * sr), dtype=np.float32)

    return np.concatenate([chirp, pad]).reshape(-1, 1)


def measure_once(out_dev, in_dev, chirp, max_delay_ms, sr):
    """One-shot: open the streams, play a chirp, record, cross-correlate. Each call
    opens and closes its own streams (see `--continuous` for the alternative that
    keeps them open across trials)."""
    sd = _sounddevice()
    out = _segment(chirp, max_delay_ms, sr)

    with sd.InputStream(device=in_dev, channels=1, samplerate=sr, dtype="float32") as istream, \
         sd.OutputStream(device=out_dev, channels=1, samplerate=sr, dtype="float32") as ostream:
        rec = _write_read(istream, ostream, out)

    return xcorr_delay(chirp, rec, 0, max_delay_ms, sr)


def _resolve_device(spec):
    """A bare integer selects a device by index (unambiguous - use this when a name
    like 'rig.monitor' matches several entries in --list); otherwise it's a name
    substring."""
    if spec is not None and spec.lstrip("-").isdigit():
        return int(spec)

    return spec


def main():
    ap = argparse.ArgumentParser(description="Live mouth-to-ear latency via chirp cross-correlation.")
    ap.add_argument("--list", action="store_true", help="list audio devices and exit")
    ap.add_argument("--out", default=None, help="output device - client A's mic feed (index from --list, or name substring)")
    ap.add_argument("--in", dest="inp", default=None, help="input device - where client B's output is captured (index or name)")
    ap.add_argument("--trials", type=int, default=15, help="number of measurements (default 15)")
    ap.add_argument("--chirp-ms", type=float, default=12.0, help="chirp length in ms (default 12)")
    ap.add_argument("--f0", type=float, default=800.0, help="chirp start frequency (default 800 Hz)")
    ap.add_argument("--f1", type=float, default=6000.0, help="chirp end frequency (default 6000 Hz)")
    ap.add_argument("--max-delay", type=float, default=500.0, help="max delay to search, ms (default 500)")
    ap.add_argument("--min-score", type=float, default=0.3, help="discard trials below this correlation (default 0.3)")
    ap.add_argument("--continuous", action="store_true",
                    help="keep the streams open across all trials and warm up first, so an idle "
                         "PipeWire monitor source can't suspend mid-run (use when the input is a "
                         "'Monitor of ...' source)")
    ap.add_argument("--warmup-ms", type=float, default=1500.0,
                    help="continuous mode: warm-up burst length that wakes the monitor and flushes "
                         "the pipeline before measuring (default 1500)")
    args = ap.parse_args()

    if args.list:
        print(_sounddevice().query_devices())
        return

    if args.out is None or args.inp is None:
        sys.exit("Need --out and --in (run with --list to see device names).")

    chirp = make_chirp(args.chirp_ms, args.f0, args.f1, SR)
    out_dev = _resolve_device(args.out)
    in_dev = _resolve_device(args.inp)

    mode = "continuous" if args.continuous else "one-shot"
    print(f"out={out_dev!r}  in={in_dev!r}  chirp={args.chirp_ms:g}ms {args.f0:g}-{args.f1:g}Hz  trials={args.trials}  ({mode})\n")

    good = []

    def record(k, delay, score):
        ok = score >= args.min_score and np.isfinite(delay)
        if ok:
            good.append(delay)
        print(f"  trial {k + 1:2d}: {delay:7.2f} ms   (corr {score:.3f})  {'ok' if ok else 'LOW - ignored'}")

    try:
        if args.continuous:
            # Streams stay open for the whole run so the output sink (and thus the
            # monitor source feeding the app) never idle-suspends between trials; a
            # warm-up burst first wakes the monitor and primes the jitter buffer.
            sd = _sounddevice()
            seg = _segment(chirp, args.max_delay, SR)
            n_warm = int(args.warmup_ms / 1000 * SR)
            warm = (0.05 * np.random.default_rng().standard_normal(n_warm)).astype(np.float32).reshape(-1, 1)

            with sd.InputStream(device=in_dev, channels=1, samplerate=SR, dtype="float32") as istream, \
                 sd.OutputStream(device=out_dev, channels=1, samplerate=SR, dtype="float32") as ostream:
                if n_warm > 0:
                    _write_read(istream, ostream, warm)

                for k in range(args.trials):
                    rec = _write_read(istream, ostream, seg)
                    delay, score = xcorr_delay(chirp, rec, 0, args.max_delay, SR)
                    record(k, delay, score)
        else:
            for k in range(args.trials):
                delay, score = measure_once(out_dev, in_dev, chirp, args.max_delay, SR)
                record(k, delay, score)
                time.sleep(0.15)  # let the buffers settle between shots
    except Exception as e:
        sys.exit(f"audio error: {e}\n(check device names with --list)")

    print()
    if not good:
        sys.exit("No reliable trials. Raise the level, disable noise suppression/AEC, or re-check routing.")

    med = statistics.median(good)
    spread = statistics.pstdev(good) if len(good) > 1 else 0.0
    print(f"  kept {len(good)}/{args.trials} trials")
    print(f"  >> MOUTH-TO-EAR (median): {med:.1f} ms   (sigma {spread:.1f} ms, min {min(good):.1f}, max {max(good):.1f})")
    print("  (subtract a baseline run for the rig's own offset; add RTT/2 for a WAN call)")


if __name__ == "__main__":
    main()
