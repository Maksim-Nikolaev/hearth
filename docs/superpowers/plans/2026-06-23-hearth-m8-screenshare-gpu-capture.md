# Hearth M8 – Safe single-capture screenshare + X11 GPU capture

> **For agentic workers:** Use superpowers:subagent-driven-development. Steps use `- [ ]`.

**Goal:** Stop the screenshare crash / whole-PC freeze on X11, then add OBS-style GPU (xcomposite+GL) capture for high-res/fps. The freeze is architectural (N+1 concurrent captures+encodes, non-leaky queues), not hardware — see memory `hearth-screenshare-resource-multiplier`. PipeWire/portal DMABUF is Wayland-only and unavailable here — see `hearth-x11-no-screencast-portal`.

**Architecture target:** ONE capture → ONE encode → fan the encoded H265 out to every viewer (`appsink` → per-peer `appsrc`), with the SAME capture tee'd to the local preview. Leaky bounded queues so pressure drops frames instead of OOM/freezing. The capture front-end sits behind a `CaptureBackend` trait chosen at runtime (`detect_capture_backend()`), so new platforms (Wayland `pipewiresrc`, Windows WGC, macOS ScreenCaptureKit) and the Phase-2 X11 `xcomposite`+GL GPU source slot in as new trait impls without touching the tee/encode/fan-out/webrtc path. Only the X11 `ximagesrc` backend is wired now; the seam exists from day one.

**Tech:** Rust, GStreamer (`tee`, `queue leaky=downstream`, `appsink`/`appsrc`, `vah265enc`, `gtk4paintablesink`), X11 (`xcomposite`, EGL/`eglCreateImageKHR`, GL). Files: `engine/src/session.rs`, `engine/src/flow_peer.rs`, `engine/src/screen/`.

## Global Constraints
- Work on `main`, one commit per task, **do not push**, no `Co-Authored-By`. Source Rust: `. "$HOME/.cargo/env"`.
- `cargo build -p engine -p desktop` 0 warnings; `cargo test` green. Don't regress M7/M7.1/m7p2.
- **The app must never be able to freeze the PC.** Every screen queue is `leaky=downstream` with a bounded `max-size-*`. No unbounded buffering anywhere on the capture/encode path.
- Automated checks use `HEARTH_CAPTURE="videotestsrc is-live=true pattern=ball ! timeoverlay ! videoconvert"`; never grab the real `:0` in automated runs. The human does real-screen run-and-observe.
- No hard resolution/fps clamp (user decision) — safety comes from leaky queues + single capture.

---

# PHASE 1 — Single-capture, leaky, crash-free (X11, ships first)

## Task 1: Leaky bounded queues + reuse one capture for the preview

**Files:** `engine/src/screen/capture.rs`, `engine/src/session.rs` (`build_preview_pipeline`, `start_share`).

**Problem:** capture chain ends in a plain blocking `! queue`; and `start_share` builds a SECOND capture just for the preview on top of the per-viewer capture.

- [ ] **Step 1:** In `capture.rs`, make the trailing queue leaky+bounded: replace `! queue` with `! queue leaky=downstream max-size-buffers=3 max-size-bytes=0 max-size-time=0`. (Drop oldest frames under pressure; cap by buffer count, not bytes, since one 2K frame > default 10 MB.) Keep the existing tests; update the assertion that checks the queue string.
- [ ] **Step 2:** Add a `ScreenSource` concept (Task 2 builds it fully). For THIS task, the minimal fix: when a share is active, the preview must NOT spin up its own `ximagesrc`. Gate `build_preview_pipeline` so that while sharing it returns the share's paintable rather than a new capture. (If Task 2 lands first, skip — Task 2 supersedes this.) If implementing standalone, document the interim behaviour.
- [ ] **Step 3:** `cargo build -p engine` 0 warnings; `cargo test -p engine` green.
- [ ] **Step 4: Commit** `git commit -m "perf(engine): leaky bounded screenshare queues; stop redundant preview capture"`

## Task 2: One capture → one encode → fan-out to peers (the core fix)

**Files:** `engine/src/screen/source.rs` (new `ScreenSource`), `engine/src/flow_peer.rs` (screen send branch → appsrc-fed), `engine/src/session.rs` (`start_share`/`stop_share`/`start_offerer`/`start_preview`).

**Design:**
- New `ScreenSource` owns ONE pipeline: `capture_chain ! tee name=t  t. ! queue(leaky) ! <encoder tuned> ! h265parse ! appsink name=enc(emit-signals, drop, max-buffers=3)  t. ! queue(leaky) ! <preview sink>`. It exposes: the preview `paintable` (glib::Object); `register_viewer(id) -> AppSrc` and `unregister_viewer(id)`; internally, the `appsink`'s `new-sample` callback pushes each encoded H265 `Buffer` to every registered viewer `AppSrc` (clone the buffer; `push-buffer`). Leaky appsink (`drop=true max-buffers=3`) so a slow consumer never stalls the encoder.
- `build_screen_send_branch` for a screen OFFERER becomes: build `appsrc(is-live, format=time, do-timestamp or copy PTS) ! h265parse ! rtph265pay config-interval=-1 ! caps(H265 pt=96) ! webrtc` and return/register the `AppSrc` with the `ScreenSource` (audio branch unchanged). No per-peer `ximagesrc`/encoder anymore.
- `start_share`: create the single `ScreenSource` (capture+encode+preview); for each viewer `start_offerer(Screen)` registers an appsrc. `start_preview` (picker, no viewers): create a `ScreenSource` in preview mode (encoder branch may run with zero viewers — buffers dropped — or be added lazily on first viewer; choose the simpler that still builds 0 warnings). `stop_share`/`stop_preview`: unregister viewers, set the ScreenSource pipeline to Null, drop it.
- Encoder tuning (`tune_encoder`) moves into `ScreenSource` (one encoder now). Bitrate from `share_config.bitrate_kbps` as today.

- [ ] **Step 1:** Implement `ScreenSource` (new file) with the tee/appsink/appsrc fan-out + leaky queues + preview paintable. Unit-test the fan-out buffer push with `HEARTH_CAPTURE` synthetic + a fake viewer appsrc (assert buffers arrive).
- [ ] **Step 2:** Rework `flow_peer.rs` screen offerer send branch to appsrc-fed + register with the source. Rework `session.rs` share/preview/offerer lifecycle to use the one `ScreenSource`.
- [ ] **Step 3:** Build 0 warnings; `cargo test -p engine` green. Run-and-observe (synthetic): one capture, viewer sees video, preview shows.
- [ ] **Step 4: Commit** `git commit -m "perf(engine): single capture+encode screenshare, fan encoded H265 out to peers"`

## Task 3: stop→start lifecycle hardening

**Files:** `engine/src/session.rs`, `engine/src/screen/source.rs`.

**Problem:** stopping then immediately restarting a share crashes (live capture torn to Null then re-grabbed).

- [ ] **Step 1:** Reproduce safely: synthetic `HEARTH_CAPTURE`, start_share → stop_share → start_share in a loop in a unit/integration test or a scripted run; capture any panic via the existing panic hook.
- [ ] **Step 2:** Make teardown fully synchronous & idempotent: on `stop_share`, unregister all viewers, set the `ScreenSource` pipeline to Null and **wait for the state change to complete** (`get_state(timeout)`) before dropping; null out the stored source so a re-`start_share` builds fresh. Ensure no appsink callback fires into a dropped viewer (guard with the registry lock / weak refs).
- [ ] **Step 3:** Build 0 warnings; test green; the start→stop→start loop no longer crashes.
- [ ] **Step 4: Commit** `git commit -m "fix(engine): idempotent synchronous screenshare teardown (stop->start no longer crashes)"`

---

# PHASE 2 — X11 GPU capture (xcomposite + EGLImage), spike first

> No stock GStreamer element does X11 zero-copy GPU capture. Prove it in isolation before integrating. OBS's `linux-capture/xcomposite-input` is the reference (redirect window/root to a pixmap; `glXBindTexImageEXT`/`eglCreateImageKHR(EGL_NATIVE_PIXMAP_KHR)`; bind as GL texture; encode on-GPU).

## Task 4: Feasibility spike — EGLImage-from-X-pixmap → GStreamer GL texture

**Files:** `engine/examples/x11_gpu_capture_spike.rs` (throwaway).

- [ ] **Step 1:** Standalone example: `XCompositeRedirectWindow` (or root) → `XCompositeNameWindowPixmap` → EGL `eglCreateImageKHR(display, ctx, EGL_NATIVE_PIXMAP_KHR, pixmap, attrs)` → `glEGLImageTargetTexture2DOES` → wrap as a `GstGLMemoryEGL`/`appsrc` GL buffer → `glcolorconvert ! vapostproc ! vah265enc ! fakesink`. Measure CPU vs the current `ximagesrc` path at 2K60/120.
- [ ] **Step 2:** Document in the report: does it work on this X11/Cinnamon + Mesa/AMD stack? CPU delta? Pitfalls (GL context sharing with GStreamer, modifiers, damage events, multi-monitor). Decide GO/NO-GO for integration.
- [ ] **Step 3: Commit** the spike + findings `git commit -m "spike(engine): X11 xcomposite+EGLImage GPU capture feasibility"`

## Task 5: Integrate GPU capture behind the ScreenSource front-end (only if spike = GO)

**Files:** `engine/src/screen/source.rs`, `engine/src/screen/capture.rs`.

- [ ] Replace the `ScreenSource` capture front-end with the GPU `xcomposite`+EGLImage source (kept zero-copy into `vapostproc ! vah265enc`), selectable via config / env, with `ximagesrc` as the fallback. Leaky queues unchanged. Detailed steps TBD from the spike's findings (this task is specced after Task 4).

---

## Self-Review
- Phase 1 (T1–T3) stops the crash/freeze on X11 and is independently shippable: leaky queues (never OOM), one capture+encode (no N+1 multiplier), clean stop→start. Delivers stable 2K30–60.
- Phase 2 (T4–T5) chases OBS-grade 2K120 via GPU capture, gated behind a spike because it's the highest-risk, non-standard piece.
- Risk: T2 is the big refactor (per-peer pipelines → shared encode + appsrc fan-out); T4/T5 are research-grade. T1 is a quick, safe immediate mitigation — land it first.
