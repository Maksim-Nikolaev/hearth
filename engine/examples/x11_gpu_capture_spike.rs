//! Feasibility spike (throwaway): X11 GPU screenshare capture.
//!
//! Redirect ONE window with XComposite, name its backing pixmap, import it as an
//! EGLImage, export that as a DMABUF, and encode it zero-copy with vah265enc.
//! Compares CPU vs the ximagesrc path. See
//! docs/superpowers/plans/2026-06-23-hearth-x11-gpu-capture-spike.md.
//!
//! Run (real screen — captures a window you point it at):
//!   cargo build -p engine --example x11_gpu_capture_spike
//!   ./target/debug/examples/x11_gpu_capture_spike --window 0x<xid>
//! Find an xid with `xwininfo` (click the target window).
//!
//! NEVER redirects the root window: Cinnamon/Muffin already composites it.

use x11rb::connection::Connection;
use x11rb::protocol::composite::{ConnectionExt as _, Redirect};
use x11rb::protocol::xproto::{ConnectionExt as _, Window};

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

    // Tasks 3+ (EGLImage -> DMABUF -> vah265enc) build on top of this.
}
