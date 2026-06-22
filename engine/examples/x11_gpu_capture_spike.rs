//! Feasibility spike (throwaway): X11 GPU screenshare capture.
//!
//! Redirect ONE window with XComposite, name its backing pixmap, import it as an
//! EGLImage, export that as a DMABUF, and (Task 4) encode it zero-copy with
//! vah265enc. See
//! docs/superpowers/plans/2026-06-23-hearth-x11-gpu-capture-spike.md.
//!
//! Run (real screen — captures a window you point it at):
//!   cargo build -p engine --example x11_gpu_capture_spike
//!   ./target/debug/examples/x11_gpu_capture_spike --window 0x<xid>
//! Find an xid with `xwininfo` (click the target window).
//!
//! NEVER redirects the root window: Cinnamon/Muffin already composites it.

use std::ffi::c_void;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

use khronos_egl as egl;
use x11rb::connection::Connection;
use x11rb::protocol::composite::{ConnectionExt as _, Redirect};
use x11rb::protocol::xproto::{ConnectionExt as _, Window};

/// EGL_NATIVE_PIXMAP_KHR — create-image target for an X pixmap (EGL_KHR_image_pixmap).
const EGL_NATIVE_PIXMAP_KHR: egl::Enum = 0x30B0;

/// A single-plane DMABUF exported from an X pixmap.
#[derive(Debug)]
#[allow(dead_code)]
struct DmabufFrame {
    fd: OwnedFd,
    fourcc: u32,
    modifier: u64,
    stride: i32,
    offset: i32,
    width: u32,
    height: u32,
}

// Mesa extension entry points (not in khronos-egl); loaded via get_proc_address.
type EglDpy = *mut c_void;
type EglImg = *mut c_void;
type QueryFn = unsafe extern "C" fn(EglDpy, EglImg, *mut i32, *mut i32, *mut u64) -> egl::Boolean;
type ExportFn = unsafe extern "C" fn(EglDpy, EglImg, *mut i32, *mut i32, *mut i32) -> egl::Boolean;

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

    // --- EGL: pixmap -> EGLImage -> DMABUF (Task 3, the crux) ---
    let egl = unsafe { egl::DynamicInstance::<egl::EGL1_5>::load_required() }.expect("load libEGL");
    let dpy = unsafe { egl.get_display(egl::DEFAULT_DISPLAY) }.expect("eglGetDisplay(DEFAULT)");
    let (major, minor) = egl.initialize(dpy).expect("eglInitialize");
    println!("EGL {major}.{minor}");

    let exts = egl
        .query_string(Some(dpy), egl::EXTENSIONS)
        .unwrap()
        .to_string_lossy()
        .into_owned();
    for needed in ["EGL_KHR_image_base", "EGL_MESA_image_dma_buf_export"] {
        assert!(exts.contains(needed), "missing EGL extension {needed}\n  have: {exts}");
    }

    let image = egl
        .create_image(
            dpy,
            unsafe { egl::Context::from_ptr(std::ptr::null_mut()) }, // EGL_NO_CONTEXT
            EGL_NATIVE_PIXMAP_KHR,
            unsafe { egl::ClientBuffer::from_ptr(pixmap as usize as *mut c_void) },
            &[egl::ATTRIB_NONE],
        )
        .expect("eglCreateImage(EGL_NATIVE_PIXMAP_KHR)");

    let query: QueryFn = unsafe {
        std::mem::transmute(egl.get_proc_address("eglExportDMABUFImageQueryMESA").expect("no query fn"))
    };
    let export: ExportFn = unsafe {
        std::mem::transmute(egl.get_proc_address("eglExportDMABUFImageMESA").expect("no export fn"))
    };

    let (mut fourcc, mut planes, mut modifier) = (0i32, 0i32, 0u64);
    assert_ne!(
        unsafe { query(dpy.as_ptr(), image.as_ptr(), &mut fourcc, &mut planes, &mut modifier) },
        egl::FALSE,
        "eglExportDMABUFImageQueryMESA failed",
    );
    assert_eq!(planes, 1, "spike handles single-plane only; got {planes} planes");

    let (mut fd, mut stride, mut offset) = (-1i32, 0i32, 0i32);
    assert_ne!(
        unsafe { export(dpy.as_ptr(), image.as_ptr(), &mut fd, &mut stride, &mut offset) },
        egl::FALSE,
        "eglExportDMABUFImageMESA failed",
    );
    assert!(fd >= 0, "invalid dmabuf fd {fd}");

    let frame = DmabufFrame {
        fd: unsafe { OwnedFd::from_raw_fd(fd) },
        fourcc: fourcc as u32,
        modifier,
        stride,
        offset,
        width: geom.width as u32,
        height: geom.height as u32,
    };
    let cc = frame.fourcc.to_le_bytes();
    println!(
        "DMABUF fd={} fourcc={}{}{}{} modifier=0x{:x} stride={} offset={} {}x{}",
        frame.fd.as_raw_fd(),
        cc[0] as char,
        cc[1] as char,
        cc[2] as char,
        cc[3] as char,
        frame.modifier,
        frame.stride,
        frame.offset,
        frame.width,
        frame.height,
    );

    std::mem::forget(conn); // keep the redirect alive for the spike's lifetime
}
