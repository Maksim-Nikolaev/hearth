# Hearth — X11 GPU screenshare capture (DMABUF → VA-API) spike

**Date:** 2026-06-23
**Status:** Design approved; spec for the Phase-2 feasibility spike.

## Goal & success target

Enable **smooth 2K120 screenshare** on X11 at low CPU. `ximagesrc` (CPU XShm
grab) is acceptable at 2K60 but its per-frame CPU/X-server cost cannot sustain
120 fps. We want an OBS-style **zero-copy GPU capture** path that hands the
captured frame straight to the VA-API HEVC encoder without a CPU round-trip.

This is **research-grade** (no stock GStreamer element does X11 zero-copy GPU
capture), so it is gated behind a feasibility spike with an explicit GO/NO-GO
before any product integration.

**GO criteria:** the spike captures a window on this box, encodes it with
`vah265enc` zero-copy, decodes back to a correct image, **sustains 2K120**, and
runs at **CPU well below the `ximagesrc` baseline**.
**NO-GO:** if the DMABUF→VA hop cannot be made to work or does not beat
`ximagesrc`, fall back to Approach B (GL texture) and re-test before integrating;
if both fail, ximagesrc stays and we revisit (e.g. Wayland-only GPU path).

## Background

Phase 1 of the M8 plan already shipped: one capture → one encode → fan encoded
H265 to per-viewer `appsrc`s, with leaky bounded queues (`807b91c`, `050a0c7`,
`e28ff53`) and the viewer-side transport fixes (`8de2817`, `89294b1`, `5a9398a`).
The whole-PC freeze (N+1 concurrent captures) is gone. What remains is the
capture front-end's CPU cost at high fps.

Relevant memory: `hearth-x11-no-screencast-portal` (X11/Cinnamon has no PipeWire
portal; DMABUF-via-portal is Wayland-only, so GPU capture here must come from
`xcomposite` + EGL), `hearth-screenshare-resource-multiplier`.

**Stack confirmed on the dev box:** X11/`:0`, AMD Radeon RX 9070 XT, Mesa 25.2
(radeonsi), VA-API HEVC encode present (`vah265enc`, `vapostproc`); GStreamer GL
elements present; `libXcomposite`/`libEGL`/`libgbm`/`libGLESv2` installed;
`x11rb` already a dependency.

## Approach

**A — DMABUF-direct to VA-API (primary).**
`xcomposite` redirect (a window) → `XCompositeNameWindowPixmap` →
`eglCreateImageKHR(EGL_NATIVE_PIXMAP_KHR)` → `eglExportDMABUFImageMESA` (fd +
stride + offset + DRM fourcc + modifier) → wrap the fd as a `GstMemory` via
`GstDmaBufAllocator` → `appsrc` with `video/x-raw(memory:DMABuf)` caps →
`vapostproc` (DMABUF import + any colorconvert/scale on the VA side) →
`vah265enc` → `appsink`. No GL pipeline, no GStreamer GL-context sharing —
lowest latency and fewest moving parts on this Mesa/AMD + modern `va` plugin
stack.

**B — OBS-style GL texture (fallback).**
…EGLImage → `glEGLImageTargetTexture2DOES` GL texture → `GstGLMemoryEGL` via a
shared `GstGLContext` → `glcolorconvert` → `vapostproc`/`gldownload` →
`vah265enc`. Proven (OBS `linux-capture/xcomposite-input`), but adds
`gstreamer-gl` + `gl` and the GL-context-sharing complexity. Only used if A's
DMABUF export/import will not negotiate.

**C — `glReadPixels` CPU readback (non-goal).** ~`ximagesrc` cost; debug only.

## Spike milestones

- **M0 — Baseline.** Measure single-capture `ximagesrc` process CPU at 2K60 and
  2K120 (and frame-drop count). Safe now that it is one leaky capture. Human runs
  on the real screen; numbers recorded as the comparison point.
- **M1 — The crux (hard GO/NO-GO gate).** Standalone throwaway example
  `engine/examples/x11_gpu_capture_spike.rs` implementing Approach A against a
  **specific window** (see refinement below). Drive frames off a target-fps tick.
  Verify the encode decodes back correctly; measure CPU at 2K60 then 2K120. If
  NO-GO, swap M1 for Approach B and re-test before proceeding.
- **M2 — Decide + report.** Document: does it work on this stack? CPU delta vs
  M0? Pitfalls (DMABUF formats/modifiers, XDamage/repaint, multi-monitor,
  window-vs-root). Explicit GO/NO-GO. Commit the spike example + findings.
- **M3 — Integration (only if GO; its own spec/plan).** Wire the GPU source
  behind `CaptureBackend`; details specced from M2's findings.

## Technical design

**The hop (Approach A):**
1. X connection via `x11rb`; query Composite; redirect the target window
   (`Automatic`); `NameWindowPixmap` → `Pixmap` that tracks the window.
2. EGL display for the X11 display; `eglCreateImageKHR(dpy, EGL_NO_CONTEXT,
   EGL_NATIVE_PIXMAP_KHR, pixmap, attrs)` (needs `EGL_KHR_image_pixmap`).
3. `eglExportDMABUFImageQueryMESA` (fourcc, num planes, modifiers) +
   `eglExportDMABUFImageMESA` (fds, strides, offsets) — Mesa extensions loaded via
   `eglGetProcAddress`.
4. Wrap the fd via `GstDmaBufAllocator`; build a buffer with the right
   `VideoMeta` + caps. The exact `memory:DMABuf` caps (DRM format + modifier) the
   `va` plugin's `vapostproc` accepts are **nailed empirically** in M1 — this is
   the fiddliest part.
5. `appsrc(is-live, format=Time) → vapostproc → vah265enc → appsink`/`filesink`.
6. Repaint: re-export each fps tick; optionally XDamage (`xfixes`) to skip
   unchanged frames.

**New dependencies (Approach A):** `x11rb` (enable `composite`, `xfixes`,
`randr`), `khronos-egl`, `gstreamer-allocators`. Approach B additionally needs
`gstreamer-gl` + `gl`. The user has accepted heavier deps — the cross-platform
roadmap (Wayland `pipewiresrc`, Windows WGC, macOS ScreenCaptureKit) requires new
native deps regardless, and best performance/latency is the priority.

**Measurement:** sample the spike process CPU from `/proc/<pid>/stat`; count
`appsink` frames vs expected (drops). Compare M0 vs M1 at 2K60 and 2K120.
Correctness: `filesink` a few seconds of H265, decode back and eyeball.

**Refinement — capture a *window*, not the root, for M1.** Cinnamon (Muffin) is
already a compositor; only one client should `XCompositeRedirect` the root, so
redirecting it ourselves risks fighting the desktop. The DMABUF interop is
identical for a window or the root, so M1 proves it on a **specific window** (no
compositor conflict, safe). Whole-screen-under-an-existing-compositor (read the
Composite Overlay Window, or coordinate) is an explicit **M3 integration
concern**, not a spike risk — this mirrors how OBS separates window capture from
full-screen.

## Integration sketch (M3, post-GO — specced separately)

**`CaptureBackend` goes programmatic.** Today `capture_chain(&cfg) -> String`
(a gst-launch substring) cannot express the GPU source. Evolve to:
```rust
trait CaptureBackend {
    fn name(&self) -> &'static str;
    /// Capture source as a bin whose src pad feeds ScreenSource's tee.
    fn build_source(&self, cfg: &ShareConfig) -> anyhow::Result<gst::Element>;
}
```
- `X11Ximage::build_source` wraps today's string via
  `parse::bin_from_description` — behaviour-preserving; the shipped path does not
  change.
- `X11GpuCapture::build_source` builds the spike's DMABUF bin (ghost-padded).
- `ScreenSource::new` links `backend.build_source(cfg)?` into the tee — one call
  site changes.
- `detect_capture_backend` picks `X11GpuCapture` when X11 + a capability probe
  passes (EGL DMABUF export + VA HEVC), else `X11Ximage`; runtime fallback to
  ximagesrc if GPU `build_source` errors.

**Encode/preview split adapts to DMABuf:** the GPU source emits `memory:DMABuf`,
so the tee feeds `vah265enc` (VA, zero-copy) on the encode branch and the preview
via dmabuf/GL import into `gtk4paintablesink` (or a cheap `vapostproc` download —
preview is fps-capped at 10). Exact wiring is specced from M2's findings.

**120 fps + safety:** add `120` to the picker presets; make `key-int-max`
fps-relative (~1 s GOP) in `tune_encoder`; **no hard res/fps clamp** — safety
stays "single capture + leaky bounded queues everywhere," now with less CPU/X
load than ximagesrc. The "app must never freeze the PC" constraint holds; the
spike itself is standalone so there is no app-freeze risk during M1.

## Risks & pitfalls (to confirm/resolve in M1–M2)

- DMABUF format/modifier negotiation between `eglExportDMABUFImageMESA` and
  `vapostproc` (AMD tiling modifiers must be carried in caps).
- Pixmap freshness / repaint cadence; XDamage integration.
- Multi-monitor and window-resize handling (re-`NameWindowPixmap` on configure).
- Root/full-screen capture under Cinnamon's compositor (deferred to M3).
- GL-context sharing — only relevant if we fall back to Approach B.

## Out of scope

- Whole-screen (root/COW) capture under the running compositor — M3.
- Product integration, detection, fallback wiring, 120 fps UI — M3 (separate
  spec, specced from spike findings).
- Wayland / Windows / macOS backends — future, but the `CaptureBackend` seam is
  shaped to accept them.
