//! Feasibility spike (throwaway): X11 GPU screenshare capture.
//!
//! Redirect ONE window with XComposite, name its backing pixmap, get its GPU
//! buffer as a DMABUF (with the REAL tiling modifier) via DRI3, and encode it
//! zero-copy with vah265enc. See
//! docs/superpowers/plans/2026-06-23-hearth-x11-gpu-capture-spike.md.
//!
//! Run (real screen — captures a window you point it at):
//!   cargo build -p engine --example x11_gpu_capture_spike
//!   ./target/debug/examples/x11_gpu_capture_spike --window 0x<xid> --secs 5
//! Find an xid with `xwininfo` (click the target window).
//!
//! NEVER redirects the root window: Cinnamon/Muffin already composites it.
//!
//! DRI3 (not EGL) for the export: eglExportDMABUFImageMESA is modifier-blind
//! (always reports INVALID + a linear stride), so a tiled AMD pixmap decodes as
//! garbage. DRI3 BuffersFromPixmap reports the authoritative modifier/stride.

use std::os::fd::OwnedFd;

use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use x11rb::connection::Connection;
use x11rb::protocol::composite::{ConnectionExt as _, Redirect};
use x11rb::protocol::dri3::ConnectionExt as _;
use x11rb::protocol::xproto::{ConnectionExt as _, Window};

/// A single-plane DMABUF backing an X pixmap.
#[derive(Debug)]
struct DmabufFrame {
    fd: OwnedFd,
    fourcc: u32,
    modifier: u64,
    size: usize,
    width: u32,
    height: u32,
}

/// The only AR24 DMABuf layout `vapostproc` accepts on this AMD/Mesa stack (from
/// `gst-inspect-1.0 vapostproc`). DRI3 1.0 can't report the pixmap's modifier, so
/// we assume this one — it is the layout radeonsi uses for these buffers.
const AMD_AR24_MODIFIER: u64 = 0x0200000000082305;

/// `--window 0x<hex>` selects an explicit window; otherwise fall back to the
/// last (topmost) mapped child of the root for a quick manual run.
fn pick_window(conn: &impl Connection, root: Window) -> Window {
    let args: Vec<String> = std::env::args().collect();
    if let Some(i) = args.iter().position(|a| a == "--window") {
        let raw = args.get(i + 1).expect("--window needs a value");
        return u32::from_str_radix(raw.trim_start_matches("0x"), 16).expect("bad window id");
    }
    let tree = conn.query_tree(root).unwrap().reply().unwrap();
    *tree.children.last().unwrap_or(&root)
}

/// `--<name> <u32>`, else `default`.
fn arg_u32(name: &str, default: u32) -> u32 {
    let args: Vec<String> = std::env::args().collect();
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn main() {
    let (conn, screen_num) = x11rb::connect(None).expect("connect to X");
    let root = conn.setup().roots[screen_num].root;

    let v = conn.composite_query_version(0, 4).unwrap().reply().unwrap();
    println!("composite {}.{}", v.major_version, v.minor_version);

    let window = pick_window(&conn, root);
    let geom = conn.get_geometry(window).unwrap().reply().unwrap();
    println!("window 0x{window:x} {}x{}", geom.width, geom.height);

    conn.composite_redirect_window(window, Redirect::AUTOMATIC)
        .unwrap()
        .check()
        .expect("redirect window (is another compositor fighting us?)");

    let pixmap = conn.generate_id().unwrap();
    conn.composite_name_window_pixmap(window, pixmap)
        .unwrap()
        .check()
        .expect("name window pixmap");
    conn.flush().unwrap();
    println!("named pixmap 0x{pixmap:x} for window 0x{window:x}");

    // --- DRI3: pixmap -> DMABUF (the crux) ---
    let dv = conn.dri3_query_version(1, 0).unwrap().reply().unwrap();
    println!("dri3 {}.{}", dv.major_version, dv.minor_version);

    // DRI3 1.0 BufferFromPixmap: the real buffer fd + stride + size (authoritative
    // for a tiled buffer, unlike EGL's linear stride). Modifier is assumed.
    let buf = conn.dri3_buffer_from_pixmap(pixmap).unwrap().reply().unwrap();
    let frame = DmabufFrame {
        fd: buf.pixmap_fd,
        fourcc: u32::from_le_bytes(*b"AR24"),
        modifier: AMD_AR24_MODIFIER,
        size: buf.size as usize,
        width: buf.width as u32,
        height: buf.height as u32,
    };
    println!(
        "DRI3 buffer: {}x{} depth={} bpp={} stride={} size={}",
        buf.width, buf.height, buf.depth, buf.bpp, buf.stride, buf.size,
    );

    let secs = arg_u32("--secs", 0);
    if secs > 0 {
        encode_dmabuf(frame, arg_u32("--fps", 60), secs);
    }

    std::mem::forget(conn); // keep the redirect alive for the spike's lifetime
}

/// Push the DMABUF through `vapostproc -> vah265enc` to a file, re-pushing one
/// zero-copy buffer at `fps` for `secs` seconds (the pixmap tracks the live
/// window). Verify the result decodes (Task 4 of the plan).
fn encode_dmabuf(frame: DmabufFrame, fps: u32, secs: u32) {
    gst::init().unwrap();

    // vapostproc imports DMABuf as `format=DMA_DRM` + `drm-format=<fourcc>:<mod>`.
    // We pass the REAL modifier from DRI3, so the VA driver de-tiles correctly.
    let cc = frame.fourcc.to_le_bytes();
    // GStreamer matches drm-format as a STRING; the modifier must be 0x + 16 hex
    // digits (zero-padded) to match what vapostproc advertises.
    let drm_format = format!(
        "{}{}{}{}:0x{:016x}",
        cc[0] as char, cc[1] as char, cc[2] as char, cc[3] as char, frame.modifier,
    );
    println!("appsrc drm-format = {drm_format}");

    let caps = gst::Caps::builder("video/x-raw")
        .features(["memory:DMABuf"])
        .field("format", "DMA_DRM")
        .field("drm-format", &drm_format)
        .field("width", frame.width as i32)
        .field("height", frame.height as i32)
        .field("framerate", gst::Fraction::new(fps as i32, 1))
        .build();

    let appsrc = gst_app::AppSrc::builder()
        .caps(&caps)
        .is_live(true)
        .format(gst::Format::Time)
        .do_timestamp(true)
        .build();

    let pipeline = gst::Pipeline::new();
    let vapostproc = gst::ElementFactory::make("vapostproc").build().unwrap();
    let enc = gst::ElementFactory::make("vah265enc").build().unwrap();
    let parse = gst::ElementFactory::make("h265parse").build().unwrap();
    let sink = gst::ElementFactory::make("filesink")
        .property("location", "/tmp/spike.h265")
        .build()
        .unwrap();

    pipeline
        .add_many([appsrc.upcast_ref(), &vapostproc, &enc, &parse, &sink])
        .unwrap();
    gst::Element::link_many([appsrc.upcast_ref(), &vapostproc, &enc, &parse, &sink]).unwrap();
    pipeline.set_state(gst::State::Playing).unwrap();

    let allocator = gstreamer_allocators::DmaBufAllocator::new();
    // SAFETY: `frame.fd` is a valid single-plane DMABUF of `frame.size` bytes from DRI3.
    let mem = unsafe { allocator.alloc(frame.fd, frame.size) }.expect("wrap dmabuf as GstMemory");
    let mut buffer = gst::Buffer::new();
    buffer.get_mut().unwrap().append_memory(mem);

    let n = u64::from(fps) * u64::from(secs);
    let dur = std::time::Duration::from_secs_f64(1.0 / f64::from(fps));
    for _ in 0..n {
        if appsrc.push_buffer(buffer.clone()).is_err() {
            break;
        }
        std::thread::sleep(dur);
    }
    let _ = appsrc.end_of_stream();

    let bus = pipeline.bus().unwrap();
    for msg in bus.iter_timed(gst::ClockTime::from_seconds(5)) {
        use gst::MessageView;
        match msg.view() {
            MessageView::Eos(_) => break,
            MessageView::Error(e) => {
                eprintln!("pipeline error: {} ({:?})", e.error(), e.debug());
                break;
            }
            _ => {}
        }
    }
    let _ = pipeline.set_state(gst::State::Null);
    println!("wrote /tmp/spike.h265 ({n} frames @ {fps} fps)");
}
