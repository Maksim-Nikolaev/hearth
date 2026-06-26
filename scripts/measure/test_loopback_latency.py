#!/usr/bin/env python3
"""
Automated tests for loopback_latency.py's measurement math.

Only the pure DSP is testable without hardware: chirp generation and the
cross-correlation delay estimator. We synthesise a recording by delaying the
chirp by a known amount, adding noise, and band-limiting it (to mimic Opus), then
assert the estimator recovers the delay. The live audio path (sd.playrec, device
routing) needs real devices and is verified by hand, not here.

Run:  pytest scripts/measure/test_loopback_latency.py
  or: python scripts/measure/test_loopback_latency.py
"""

import os
import sys

import numpy as np
import scipy.signal as sig

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from loopback_latency import SR, make_chirp, xcorr_delay  # noqa: E402


def _synth_recording(chirp, delay_ms, sr, gain=0.6, noise=0.02, bandlimit=True, seed=0):
    """A recording that contains the chirp delayed by `delay_ms`, with noise and
    (optionally) an 8 kHz low-pass to mimic a codec/AGC mangling the signal."""
    rng = np.random.default_rng(seed)
    rec = np.zeros(len(chirp) + int(0.55 * sr), dtype=np.float32)

    d = int(round(delay_ms / 1000 * sr))
    rec[d:d + len(chirp)] += gain * chirp
    rec += (noise * rng.standard_normal(len(rec))).astype(np.float32)

    if bandlimit:
        b, a = sig.butter(6, 8000 / (sr / 2))
        rec = sig.filtfilt(b, a, rec).astype(np.float32)

    return rec


def test_recovers_known_integer_delay():
    chirp = make_chirp(12, 800, 6000, SR)
    rec = _synth_recording(chirp, 37.0, SR)

    est, score = xcorr_delay(chirp, rec, 0, 500, SR)

    assert abs(est - 37.0) < 0.5, f"got {est:.2f} ms"
    assert score > 0.5, f"weak correlation {score:.3f}"


def test_recovers_sub_sample_delay():
    # 63.5 ms is not on a sample boundary (48 kHz -> 1 sample = ~0.0208 ms); the
    # parabolic refinement must land within a fraction of a millisecond.
    chirp = make_chirp(12, 800, 6000, SR)
    rec = _synth_recording(chirp, 63.5, SR)

    est, _ = xcorr_delay(chirp, rec, 0, 500, SR)

    assert abs(est - 63.5) < 0.5, f"got {est:.2f} ms"


def test_accuracy_across_the_search_range():
    chirp = make_chirp(12, 800, 6000, SR)
    for true_ms in (5.0, 80.0, 150.0, 300.0):
        rec = _synth_recording(chirp, true_ms, SR, seed=int(true_ms))
        est, score = xcorr_delay(chirp, rec, 0, 500, SR)

        assert abs(est - true_ms) < 1.0, f"{true_ms} ms -> {est:.2f} ms"
        assert score > 0.5


def test_pure_noise_scores_low():
    # No chirp present -> the estimator must report a weak correlation so the CLI's
    # --min-score gate discards the trial rather than inventing a delay.
    chirp = make_chirp(12, 800, 6000, SR)
    rng = np.random.default_rng(1)
    rec = (0.05 * rng.standard_normal(len(chirp) + int(0.55 * SR))).astype(np.float32)

    _, score = xcorr_delay(chirp, rec, 0, 500, SR)

    assert score < 0.3, f"noise correlated too strongly: {score:.3f}"


def test_chirp_is_windowed_and_unit_scaled():
    chirp = make_chirp(12, 800, 6000, SR)

    assert len(chirp) == int(12 / 1000 * SR)
    assert abs(chirp[0]) < 1e-6 and abs(chirp[-1]) < 1e-6, "edges must taper to zero"
    assert np.max(np.abs(chirp)) <= 1.0


if __name__ == "__main__":
    failed = 0
    for name, fn in sorted(globals().items()):
        if name.startswith("test_") and callable(fn):
            try:
                fn()
                print(f"PASS  {name}")
            except AssertionError as e:
                failed += 1
                print(f"FAIL  {name}: {e}")

    print(f"\n{'ALL PASSED' if not failed else f'{failed} FAILED'}")
    sys.exit(1 if failed else 0)
