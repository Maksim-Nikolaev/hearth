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
## M1 import — DMABuf → VA-API: **garbage (mis-detiled)**

- `vapostproc` DMABuf import accepts ONLY `format=DMA_DRM`,
  `drm-format=AR24:0x0200000000082305` (the AMD tiled modifier) — no linear/implicit form.
  drm-format is matched as a STRING, so the modifier must be `0x` + 16 zero-padded hex digits.
- Switched export from EGL to **DRI3**: server is **DRI3 1.0** only (no `BuffersFromPixmap`,
  so no modifier). `BufferFromPixmap` (1.0) gives fd + `stride=10240`, `size=14336000`
  = exactly `2560×4×1400` (linear dims, no tile padding).
- Negotiation + encode + decode all succeed once the modifier string matches, but the decoded
  PNG is **garbage** (window content present but mis-detiled) — with both the EGL fd and the
  DRI3 fd, claiming the tiled modifier.

## Verdict — Approach A (direct DMABuf → VA-API): **NO-GO on this host**

- **Root cause:** the pixmap's real tiling modifier cannot be obtained here.
  `eglExportDMABUFImageMESA` is modifier-blind (reports INVALID); DRI3 1.2 (which returns the
  modifier) is unavailable (Xorg advertises DRI3 1.0). `vapostproc` requires the exact tiled
  modifier, so we must guess it, and the guess doesn't match the buffer's actual layout → garbage.
- **What worked (plumbing is fine):** xcomposite redirect + NameWindowPixmap;
  `eglCreateImage(EGL_NATIVE_PIXMAP_KHR)`; DRI3 1.0 fd; the whole
  appsrc(DMABuf)→vapostproc→vah265enc→file→decode chain. Only the de-tiling is wrong.
- **Approach B (EGLImage → GL texture → GstGL→VA bridge)** would de-tile correctly via the
  driver (no modifier guessing, OBS-style), but it's the plan's highest-risk piece (thin Rust
  bindings, manual EGL/GL + shared GstGLContext) with poor ROI for the narrow "X11 + >60fps" case.

- **FINAL DECISION (user, 2026-06-23):**
  - X11 ships **CPU `ximagesrc`** capture; **>60fps on X11 is dismissed** (not a target). The
    picker already caps X11 at 60; M8 Phase 1 made it stable.
  - GPU capture is pursued on **Wayland** (xdg-desktop-portal ScreenCast + `pipewiresrc` →
    DMABuf *with* modifier → direct VA import) and native Windows/macOS, behind the
    `CaptureBackend` seam. This is the active next task.
  - Approach B on X11 is **not pursued.** This throwaway spike example is **kept** for
    re-discovery.
