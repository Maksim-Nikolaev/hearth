# Voice latency on Linux (Kubuntu/Wayland) — per-filter findings

_Measured 2026-06-24 on Kubuntu/Wayland (PipeWire 1.6.2, Logitech PRO X 2)._

End-to-end **mouth→system acoustic** latency of the Linux voice path
(`pulsesrc` → `webrtc-audio-processing` DSP → Opus → RTP/UDP → `autoaudiosink`),
measured with `scripts/measure/audio_delay.py` from a split-track OBS recording
(mic-only vs system-only). The reported number is the **envelope cross-correlation**
estimate (confidence ≥0.99 except where noted); raw cross-corr and onset spacing
agree within a frame.

Test method: alice + bob both join the call on one machine; bob muted (hear, no
echo); change one DSP setting on alice mid-call; speak short transients.

## Results

| Config | Delay (env xcorr) | Cost over baseline |
|---|---|---|
| **Filters off (baseline)** | **~7.1 ms** | — |
| VAD only | 7.38 ms | ~0 (gate decision, no buffering) |
| AGC only | 7.60 ms | ~0 (envelope gain, no buffering) |
| NS low | 14.09 ms | ~+7 ms |
| NS med | 14.54 ms | ~+7 ms |
| NS high | 13.75 ms | ~+7 ms (level doesn't change latency) |
| AEC only | 14.62 ms | ~+7 ms |
| All (NS-high + AEC + AGC + VAD) | ~20.5–20.8 ms | ~+13.5 ms (NS + AEC additive) |
| Self-monitor playback (self-hear) | 37.6 ms (conf 0.90) | separate path; acceptable |

## What this tells us

- **The device + transport path is already near-optimal (~7 ms).** The PulseAudio-
  compat shim is *not* a meaningful latency cost, so **native PipeWire small-quantum
  is a robustness nicety, not a latency win** (revises the earlier assumption).
- **NS and AEC each add ~one 10 ms processing frame (~7 ms), roughly additive.**
  VAD and AGC are effectively free. NS aggressiveness (low/med/high) changes
  CPU/quality, **not** latency.
- Even with **full processing**, ~20 ms mouth→system beats the Windows WASAPI
  floor (36–50 ms). With only the cheap filters (VAD+AGC) you stay near 7–8 ms.

## The latency lever, if we want processing on *and* lower latency

Reducing the NS/AEC frame cost, via either:
1. A low-delay configuration of `webrtc-audio-processing` (if exposed), or
2. Porting the Windows pure-Rust suite (`nnnoiseless` + `aec-rs`) to Linux —
   converges the two platforms onto one DSP codebase and may shave a frame.

## Caveats (measurements may still be imperfect)

- **Localhost**: network ≈ 0. Real internet adds RTT/2 + jitter-buffer depth on
  top of every number here.
- **onset-spacing** was noisy for NS/AEC (negative outliers from transient
  misalignment); the **envelope cross-corr** (corr ≥0.99) is the estimate to trust.
- `self-hear` had lower confidence (0.90) — AEC likely partly cancels the
  self-monitor playback, smearing the correlation.
- Single machine, ~one short clip per config; treat as order-of-magnitude (±1 frame).
