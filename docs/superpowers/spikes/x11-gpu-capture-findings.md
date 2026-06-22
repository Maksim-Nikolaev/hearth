# X11 GPU capture spike — findings

## M0 baseline (ximagesrc → vah265enc), 2560×1440

| fps | process CPU% | result | notes |
|-----|--------------|--------|-------|
| 60  | 39%          | ran 20s OK | ximagesrc→videoconvert→videoscale→videorate→vah265enc→fakesink; AMD RX 9070 XT, Mesa 25.2 |
| 120 | n/a (crashed) | **SIGSEGV** | stock CPU pipeline segfaults right after PLAYING at 120 fps (reproduced twice). CPU path is non-viable at 2K120 — strong motivation for GPU capture. |

## M1 — DMABUF export — **WORKS**

- EGL display: `eglGetDisplay(EGL_DEFAULT_DISPLAY)` → EGL 1.5 (no platform-display needed).
- Exts present: `EGL_KHR_image_base`, `EGL_MESA_image_dma_buf_export`.
- Export (2560×1400 window): `fourcc=AR24` (DRM ARGB8888), single plane, `stride=10240` (= 2560×4), `offset=0`.
- **Modifier = `0x00ffffffffffffff` = DRM_FORMAT_MOD_INVALID** — no explicit tiling reported. Same-GPU same-driver export→import should still work; the risk to watch in Task 4 is vapostproc misreading the layout as linear → garbage.
- vapostproc caps form accepted (DMA_DRM / explicit): _TBD Task 4_
- decode-back verification (Task 4 Step 4): _TBD Task 4_

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
