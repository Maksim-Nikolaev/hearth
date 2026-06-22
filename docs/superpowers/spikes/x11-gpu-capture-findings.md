# X11 GPU capture spike — findings

## M0 baseline (ximagesrc → vah265enc), 2560×1440

| fps | process CPU% | result | notes |
|-----|--------------|--------|-------|
| 60  | 39%          | ran 20s OK | ximagesrc→videoconvert→videoscale→videorate→vah265enc→fakesink; AMD RX 9070 XT, Mesa 25.2 |
| 120 | n/a (crashed) | **SIGSEGV** | stock CPU pipeline segfaults right after PLAYING at 120 fps (reproduced twice). CPU path is non-viable at 2K120 — strong motivation for GPU capture. |

## M1 — DMABUF export

- EGL display platform used:
- DRM fourcc / modifier observed:
- vapostproc caps form accepted (DMA_DRM / explicit):
- decode-back verification (Task 4 Step 4): PASS / FAIL

## M1c — GPU path CPU, 2560×1440

| fps | process CPU% | encoded / expected | sustains? |
|-----|--------------|--------------------|-----------|
| 60  |              |                    |           |
| 120 |             |                    |           |

## Verdict
- DMABUF export: <works / fails: reason>
- CPU @2K60: GPU __% vs ximagesrc __%
- CPU @2K120: GPU __% (sustains? Y/N) vs ximagesrc __%
- Pitfalls hit:
- DECISION: GO (proceed to M3 integration spec) / NO-GO (pivot to Approach B GL texture)
