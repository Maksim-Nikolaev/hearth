# Hearth desktop client

Pure-Rust GTK4 + relm4 desktop app for Hearth. Calls the `engine` crate directly
(no language bridge); GTK and GStreamer share the GLib main loop.

## Running

```bash
# backend must be up (see ../backend), with a user to log in as
cargo run -p desktop
```

Config via env (defaults shown):

| var          | default                 |
|--------------|-------------------------|
| `HEARTH_HTTP`| `http://127.0.0.1:8080` |
| `HEARTH_WS`  | `ws://127.0.0.1:8080`   |
| `HEARTH_TOKEN` | (none) – if set, auto-connects with this token, skipping login |

The login token is persisted in the OS keyring (Secret Service / Credential
Manager), falling back to a file under the config dir when no keyring is
available.

## In-window video plugin

In-window video uses GStreamer's `gtk4paintablesink` (from `gst-plugins-rs`),
which is not packaged on this distro. It was built locally and installed to
`~/.local/lib/hearth-gst-plugins/`. `main.rs` prepends that directory to
`GST_PLUGIN_PATH` automatically when it exists, so no manual export is needed in
dev. For a real install the plugin would be bundled.

## Verification log

### T7 – login to room (2026-06-22)

Auto-connect via `HEARTH_TOKEN` (a minted alice token) against the running
backend. **Result: the window reaches the "Room: main" screen** and the status
line shows the `ChatHistory([])` event – confirming login → `Session::start` →
inbound pump → `handle` → event → UI end-to-end. (A transient
`Child name 'connecting' not found` GTK warning appears at startup and is
harmless.)
