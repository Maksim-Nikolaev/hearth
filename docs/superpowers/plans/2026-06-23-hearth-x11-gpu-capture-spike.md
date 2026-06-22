# X11 GPU Capture (DMABUF → VA-API) Spike — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax.
>
> **This is a research spike.** Steps are "build a small increment → run → verify an observable milestone," not red-green TDD. The EGL→DMABUF→VA hop (Task 3) is the core unknown; its steps give real API entry points + the empirical resolution approach. The deliverable of the whole plan is a GO/NO-GO decision + report, not shippable product code.

**Goal:** Prove (or disprove) that an X11 window can be captured zero-copy on the GPU (`xcomposite` pixmap → EGL DMABUF) and encoded with `vah265enc` at 2K120, at CPU well below `ximagesrc`.

**Architecture:** A standalone throwaway example, `engine/examples/x11_gpu_capture_spike.rs`. Redirect one window with XComposite, name its backing pixmap, import it as an `EGLImage`, export that as a DMABUF fd via Mesa's `EGL_MESA_image_dma_buf_export`, wrap the fd as a `GstMemory` (`GstDmaBufAllocator`), push it through `appsrc → vapostproc → vah265enc`. Compare CPU vs an `ximagesrc` baseline.

**Tech Stack:** Rust, GStreamer 0.23 (`gstreamer`, `gstreamer-app`, `gstreamer-allocators`), `x11rb` (composite/xfixes/randr), `khronos-egl`, Mesa EGL extensions (`EGL_KHR_image_pixmap`, `EGL_MESA_image_dma_buf_export`), VA-API (`vapostproc`, `vah265enc`).

## Global Constraints

- Work on `main`, one commit per task, **do not push**, no `Co-Authored-By`. Source Rust env: `. "$HOME/.cargo/env"`.
- `cargo build -p engine --examples` must be **0 warnings**; `cargo test -p engine` stays green (the example must not break the crate build).
- The example is **throwaway** (an `examples/` binary): it must not be imported by `engine`'s library, and its spike-only deps go in `[dev-dependencies]`.
- It captures the **real screen** — only ever a **single window** (never redirect the root: Cinnamon/Muffin already composites). The human runs it and observes; agents never grab `:0` unattended.
- Target metric: total process CPU% (capture+convert+encode) at 2560×1440 @ 60 and @ 120, plus encoded-frame count vs expected (drops).

---

### Task 1: ximagesrc baseline (M0)

Establish the CPU number the GPU path must beat. No new code — a measured `gst-launch` run, recorded in the report file.

**Files:**
- Create: `docs/superpowers/spikes/x11-gpu-capture-findings.md` (the running report)

- [ ] **Step 1: Create the findings file with a Baseline section**

```markdown
# X11 GPU capture spike — findings

## M0 baseline (ximagesrc → vah265enc), 2560×1440

| fps | process CPU% | encoded frames / expected | notes |
|-----|--------------|---------------------------|-------|
| 60  |              |                           |       |
| 120 |             |                           |       |
```

- [ ] **Step 2: Measure 2K60.** In a terminal, run (human, real screen) and read CPU from a second terminal with `top -b -d1 -p "$(pgrep -n gst-launch-1.0)" | rg gst-launch`:

Run:
```bash
GST_GL_API= timeout 20 gst-launch-1.0 -e \
  ximagesrc use-damage=false ! videoconvert ! videoscale ! videorate ! \
  video/x-raw,width=2560,height=1440,framerate=60/1 ! \
  vah265enc ! fakesink sync=false
```
Record the steady-state CPU% into the table.

- [ ] **Step 3: Measure 2K120.** Same command with `framerate=120/1`. Record CPU% and whether it sustains 120 (watch for `videorate` dropping). Expected: CPU markedly higher than 60; 120 may not be sustainable — that is the point.

- [ ] **Step 4: Commit**

```bash
git add docs/superpowers/spikes/x11-gpu-capture-findings.md
git commit -m "spike(screen): record ximagesrc capture CPU baseline (M0)"
```

---

### Task 2: Spike scaffold + window selection (M1, part 1)

Add deps, create the example, select a target window, redirect it with XComposite, and obtain its backing pixmap. Observable milestone: prints the chosen window's geometry + a valid pixmap id.

**Files:**
- Modify: `engine/Cargo.toml` (add x11rb features + dev-deps)
- Create: `engine/examples/x11_gpu_capture_spike.rs`

**Interfaces:**
- Produces: a `main` that takes `--window <hex-xid>` (or `--pick` to grab the pointer-selected window) and holds an open `x11rb` connection, the target `Window`, and its composite `Pixmap`.

- [ ] **Step 1: Add dependencies to `engine/Cargo.toml`.** Add the features to the existing `x11rb` line and a dev-deps block:

```toml
x11rb = { version = "0.13", features = ["composite", "xfixes", "randr"] }

[dev-dependencies]
khronos-egl = { version = "6", features = ["dynamic"] }
gstreamer-allocators = "0.23"
libloading = "0.8"
```

- [ ] **Step 2: Create `engine/examples/x11_gpu_capture_spike.rs` with X setup.** Connect, query the Composite extension version, parse `--window`/`--pick`, redirect the window, name its pixmap:

```rust
use x11rb::connection::Connection;
use x11rb::protocol::composite::{ConnectionExt as _, Redirect};
use x11rb::protocol::xproto::{ConnectionExt as _, Window};

fn pick_window(conn: &impl Connection, root: Window) -> Window {
    // --window <hex>; else fall back to the root's first mapped child for the spike.
    let args: Vec<String> = std::env::args().collect();
    if let Some(i) = args.iter().position(|a| a == "--window") {
        return u32::from_str_radix(args[i + 1].trim_start_matches("0x"), 16).unwrap();
    }
    let tree = conn.query_tree(root).unwrap().reply().unwrap();
    *tree.children.last().unwrap_or(&root)
}

fn main() {
    let (conn, screen_num) = x11rb::connect(None).unwrap();
    let root = conn.setup().roots[screen_num].root;

    let comp = conn.composite_query_version(0, 4).unwrap().reply().unwrap();
    println!("composite version {}.{}", comp.major_version, comp.minor_version);

    let window = pick_window(&conn, root);
    let geom = conn.get_geometry(window).unwrap().reply().unwrap();
    println!("window 0x{window:x} {}x{}", geom.width, geom.height);

    conn.composite_redirect_window(window, Redirect::AUTOMATIC).unwrap();
    let pixmap = conn.generate_id().unwrap();
    conn.composite_name_window_pixmap(window, pixmap).unwrap();
    conn.flush().unwrap();
    println!("named pixmap 0x{pixmap:x}");

    // Tasks 3+ continue from here.
    std::mem::forget(conn); // keep the redirect alive for the spike's lifetime
}
```

- [ ] **Step 3: Build the example.**

Run: `. "$HOME/.cargo/env" && cargo build -p engine --example x11_gpu_capture_spike`
Expected: compiles, 0 warnings.

- [ ] **Step 4: Run against a window (human, real screen).** Open a terminal/editor, find its id with `xwininfo` (click it), then:

Run: `./target/debug/examples/x11_gpu_capture_spike --window 0x<id>`
Expected: prints composite version, the window geometry, and a named pixmap id. No X error.

- [ ] **Step 5: Commit**

```bash
git add engine/Cargo.toml engine/examples/x11_gpu_capture_spike.rs
git commit -m "spike(screen): xcomposite redirect + name-window-pixmap scaffold (M1)"
```

---

### Task 3: Pixmap → EGLImage → DMABUF export (M1, part 2 — THE CRUX)

The core unknown. Import the pixmap as an `EGLImage` and export it as a DMABUF via Mesa. Observable milestone: prints a valid DMABUF fd, DRM fourcc, modifier, stride, offset for the window's pixmap. **If this cannot be made to work, the spike pivots to Approach B (GL texture) — record that in the report and stop here.**

**Files:**
- Modify: `engine/examples/x11_gpu_capture_spike.rs`

**Interfaces:**
- Produces: `struct DmabufFrame { fd: std::os::fd::OwnedFd, fourcc: u32, modifier: u64, stride: i32, offset: i32, width: u32, height: u32 }` and `fn export_pixmap_dmabuf(...) -> DmabufFrame` used by Task 4.

- [ ] **Step 1: Initialise EGL and verify the two required extensions.** Add to the example:

```rust
use khronos_egl as egl;

const EGL_NATIVE_PIXMAP_KHR: egl::Enum = 0x30B0;
const PLATFORM_X11: egl::Enum = 0x31D5; // EGL_PLATFORM_X11_KHR

// Returns (egl instance, display). Panics with a clear message if an extension is missing.
fn init_egl(x11_display_ptr: *mut std::ffi::c_void) -> (egl::Instance<egl::Dynamic<libloading::Library, egl::EGL1_5>>, egl::Display) {
    let lib = unsafe { libloading::Library::new("libEGL.so.1") }.unwrap();
    let egl = unsafe { egl::DynamicInstance::<egl::EGL1_5>::load_required_from(lib) }.unwrap();
    let dpy = unsafe { egl.get_platform_display(PLATFORM_X11, x11_display_ptr, &[egl::ATTRIB_NONE]) }.unwrap();
    egl.initialize(dpy).unwrap();
    let exts = egl.query_string(Some(dpy), egl::EXTENSIONS).unwrap().to_string_lossy();
    for needed in ["EGL_KHR_image_pixmap", "EGL_MESA_image_dma_buf_export"] {
        assert!(exts.contains(needed), "missing required EGL extension: {needed}");
    }
    (egl, dpy)
}
```

Note: `x11rb` is XCB-based; obtain the Xlib `Display*` EGL wants by opening one Xlib display alongside (`x11-dl`) **or** use `EGL_PLATFORM_XCB` if the driver advertises it. Resolve empirically in Step 4: try X11 platform with an Xlib `Display*` first (most portable on Mesa); if `get_platform_display` fails, try `EGL_PLATFORM_XCB_EXT` (0x31DC) with the xcb connection pointer.

- [ ] **Step 2: Create the EGLImage from the pixmap and load the MESA export fns.** `eglExportDMABUFImage[Query]MESA` are not in `khronos-egl`; load them via `get_proc_address`:

```rust
type QueryFn = unsafe extern "C" fn(egl::EGLDisplay, egl::EGLImage, *mut i32, *mut i32, *mut u64) -> egl::Boolean;
type ExportFn = unsafe extern "C" fn(egl::EGLDisplay, egl::EGLImage, *mut i32, *mut i32, *mut i32) -> egl::Boolean;

fn export_pixmap_dmabuf(egl: &egl::Instance<impl egl::api::EGL1_5>, dpy: egl::Display, pixmap: u32, w: u32, h: u32) -> DmabufFrame {
    let image = unsafe {
        egl.create_image(dpy, egl::Context::from_ptr(egl::NO_CONTEXT),
            EGL_NATIVE_PIXMAP_KHR as _, egl::ClientBuffer::from_ptr(pixmap as _), &[egl::ATTRIB_NONE])
    }.expect("eglCreateImage(EGL_NATIVE_PIXMAP_KHR) failed");

    let query: QueryFn = unsafe { std::mem::transmute(egl.get_proc_address("eglExportDMABUFImageQueryMESA").unwrap()) };
    let export: ExportFn = unsafe { std::mem::transmute(egl.get_proc_address("eglExportDMABUFImageMESA").unwrap()) };

    let (mut fourcc, mut num_planes, mut modifier) = (0i32, 0i32, 0u64);
    assert_ne!(unsafe { query(dpy.as_ptr(), image.as_ptr(), &mut fourcc, &mut num_planes, &mut modifier) }, 0, "query failed");
    assert_eq!(num_planes, 1, "spike handles single-plane only; got {num_planes}");

    let (mut fd, mut stride, mut offset) = (-1i32, 0i32, 0i32);
    assert_ne!(unsafe { export(dpy.as_ptr(), image.as_ptr(), &mut fd, &mut stride, &mut offset) }, 0, "export failed");
    assert!(fd >= 0, "invalid dmabuf fd");

    use std::os::fd::FromRawFd;
    DmabufFrame { fd: unsafe { std::os::fd::OwnedFd::from_raw_fd(fd) }, fourcc: fourcc as u32, modifier, stride, offset, width: w, height: h }
}
```

- [ ] **Step 3: Call it from `main` and print the result.** After naming the pixmap:

```rust
let (egl, dpy) = init_egl(/* display ptr, resolved in Step 4 */);
let frame = export_pixmap_dmabuf(&egl, dpy, pixmap, geom.width as u32, geom.height as u32);
let cc = frame.fourcc.to_le_bytes();
println!("DMABUF fd={} fourcc={}{}{}{} modifier=0x{:x} stride={} offset={}",
    i32::from(&frame.fd), cc[0] as char, cc[1] as char, cc[2] as char, cc[3] as char,
    frame.modifier, frame.stride, frame.offset);
```

- [ ] **Step 4: Build, run, and resolve the display-platform question empirically.**

Run: `cargo build -p engine --example x11_gpu_capture_spike` then `./target/debug/examples/x11_gpu_capture_spike --window 0x<id>`
Expected (GO): prints a DMABUF line with `fd>=0`, a 4-char fourcc (likely `AR24`/`XR24` = ARGB/XRGB8888), and an AMD tiling `modifier` (non-zero). Record fourcc + modifier in the findings file — Task 4 needs them.
If `get_platform_display` or `create_image` fails: try the XCB platform fallback noted in Step 1; if both fail, write "Approach A NO-GO: <error>" in the findings file and stop (pivot to Approach B in a follow-up).

- [ ] **Step 5: Commit**

```bash
git add engine/examples/x11_gpu_capture_spike.rs docs/superpowers/spikes/x11-gpu-capture-findings.md
git commit -m "spike(screen): export xcomposite pixmap as DMABUF via EGL_MESA (M1 crux)"
```

---

### Task 4: DMABUF → GStreamer → vah265enc, encode + verify (M1, part 3)

Wrap the DMABUF as a `GstBuffer`, push it through `appsrc → vapostproc → vah265enc`, capture a few seconds at the target fps, and verify the output decodes to the window's image.

**Files:**
- Modify: `engine/examples/x11_gpu_capture_spike.rs`

**Interfaces:**
- Consumes: `export_pixmap_dmabuf` / `DmabufFrame` from Task 3.

- [ ] **Step 1: Build the encode pipeline with a DMABuf-fed appsrc.** Use the fourcc/modifier recorded in Task 4 Step... (from Task 3 Step 4) to form the caps. Add:

```rust
use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use gstreamer_allocators::DmaBufAllocator;

// caps resolved empirically (Step 4): start with the modern DMA_DRM form.
fn appsrc_caps(frame: &DmabufFrame) -> gst::Caps {
    let drm = format!("{}{}{}{}:0x{:x}",
        (frame.fourcc & 0xff) as u8 as char, ((frame.fourcc >> 8) & 0xff) as u8 as char,
        ((frame.fourcc >> 16) & 0xff) as u8 as char, ((frame.fourcc >> 24) & 0xff) as u8 as char,
        frame.modifier);
    gst::Caps::builder("video/x-raw")
        .features(["memory:DMABuf"])
        .field("format", "DMA_DRM")
        .field("drm-format", drm)
        .field("width", frame.width as i32)
        .field("height", frame.height as i32)
        .field("framerate", gst::Fraction::new(0, 1)) // variable; appsrc do-timestamp
        .build()
}
```

- [ ] **Step 2: Wrap the fd and push frames on an fps tick.** Build `appsrc(is-live, format=Time, do-timestamp) ! vapostproc ! vah265enc ! h265parse ! filesink location=/tmp/spike.h265`, then in a loop at the target fps: re-`composite_name_window_pixmap` (cheap; picks up new content), export, wrap, push:

```rust
let allocator = DmaBufAllocator::new();
let mem = unsafe { allocator.alloc_with_flags(frame.fd, size as usize, gst::MemoryFlags::empty()) }.unwrap();
let mut buf = gst::Buffer::new();
buf.get_mut().unwrap().append_memory(mem);
appsrc.push_buffer(buf).unwrap();
```
(Push for ~5 s at 60 fps. `do-timestamp` stamps each buffer; reuse the per-viewer 0-origin lesson if PTS misbehaves.)

- [ ] **Step 3: Build + run, capturing 5 s.**

Run: `cargo build -p engine --example x11_gpu_capture_spike && ./target/debug/examples/x11_gpu_capture_spike --window 0x<id> --secs 5`
Expected: `/tmp/spike.h265` is non-empty.
If caps negotiation fails (`not-negotiated`): in `appsrc_caps`, fall back to the legacy explicit form (`format=<gst-format from fourcc>` without `DMA_DRM`) and retry — record which form `vapostproc` accepted in the findings file.

- [ ] **Step 4: Verify the encode decodes to the right image.**

Run:
```bash
gst-launch-1.0 -e filesrc location=/tmp/spike.h265 ! h265parse ! avdec_h265 ! \
  videoconvert ! pngenc snapshot=true ! filesink location=/tmp/spike.png
xdg-open /tmp/spike.png
```
Expected: the PNG shows the captured window's contents (not garbage/black). Record PASS/FAIL.

- [ ] **Step 5: Commit**

```bash
git add engine/examples/x11_gpu_capture_spike.rs docs/superpowers/spikes/x11-gpu-capture-findings.md
git commit -m "spike(screen): encode DMABUF window capture with vah265enc, verify decode (M1)"
```

---

### Task 5: 2K120 CPU measurement + GO/NO-GO report (M1c + M2)

Run the GPU path at 2560×1440 @ 60 and @ 120 against a 2K window, measure CPU + drops, fill the findings table, and write the explicit decision.

**Files:**
- Modify: `engine/examples/x11_gpu_capture_spike.rs` (add `--fps`, frame counter), `docs/superpowers/spikes/x11-gpu-capture-findings.md`

- [ ] **Step 1: Add `--fps <n>` and an encoded-frame counter.** Count buffers leaving `vah265enc` (pad probe on its src) and print `encoded N / expected M` at exit.

- [ ] **Step 2: Measure 2K60.** Maximise/size the target window to ~2560×1440. Run for 20 s at `--fps 60`; read process CPU% from `top -b -d1 -p "$(pgrep -n x11_gpu_capture_spike)"`. Record CPU% + encoded/expected.

- [ ] **Step 3: Measure 2K120.** Same at `--fps 120`. Record CPU% + whether it sustains 120 (encoded ≈ expected). Compare both to the M0 baseline.

- [ ] **Step 4: Write the verdict** in the findings file:

```markdown
## Verdict
- DMABUF export: <works / fails: reason>
- Caps form vapostproc accepted: <DMA_DRM / explicit>
- CPU @2K60: GPU <x>% vs ximagesrc <y>%
- CPU @2K120: GPU <x>% (sustains? Y/N) vs ximagesrc <y>%
- Pitfalls hit: <modifiers / repaint / multi-monitor / resize>
- DECISION: GO (proceed to M3 integration spec) / NO-GO (pivot to Approach B GL texture)
```

- [ ] **Step 5: Commit**

```bash
git add engine/examples/x11_gpu_capture_spike.rs docs/superpowers/spikes/x11-gpu-capture-findings.md
git commit -m "spike(screen): 2K120 CPU measurement + GO/NO-GO verdict (M2)"
```

---

## Self-Review

**Spec coverage:** M0 baseline → Task 1. M1 (export crux) → Tasks 2–3. M1 encode+verify → Task 4. M1c CPU + M2 report/GO-NO-GO → Task 5. Window-not-root refinement → Task 2 (single window only). Deps → Task 2. Approach-B pivot → Task 3 Step 4 / Task 5 verdict. M3 integration is explicitly out of scope (its own spec, post-GO) — not planned here, by design.

**Placeholder scan:** Remaining unknowns (EGL display platform, exact `vapostproc` DMABuf caps) are framed as **empirical resolution steps with concrete candidates to try and what to record**, not "TODO" — appropriate and unavoidable for a spike's research surface.

**Type consistency:** `DmabufFrame` fields defined in Task 3 are consumed unchanged in Task 4 (`fd`, `fourcc`, `modifier`, `stride`, `width`, `height`). `export_pixmap_dmabuf` signature is stable across Tasks 3–4.
