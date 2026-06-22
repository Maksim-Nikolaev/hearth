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

### T8 – presence + chat (2026-06-22)

Two instances (`HEARTH_TITLE=alice|bob`, distinct app ids). Each window shows
the other in the **Online** list and the shared **chat history**; live messages
sent from one appear in the other and persist.

### T9 – in-window video + share/voice controls (2026-06-22)

Room toolbar: **Share screen / Call / Mute / Deafen / Stop** + a connection-state
chip. alice clicks **Share screen** → bob's window renders **alice's screen
in-window** (via `gtk4paintablesink` → `gtk::Picture`), status shows
`Screen: Connected`. Per-flow transports: stopping one flow leaves chat (and any
voice) running. Voice itself was loopback-verified at the engine level (T5).

## M6 – Discord-style group experience

### M6 T5 – dark workspace shell + login extraction (2026-06-22)

Auto-connect via `HEARTH_TOKEN` (minted alice token) against the running backend.
**Result: a dark 3-pane window appears after auto-connect** – left **CHANNELS**
rail and right **MEMBERS** rail on the darker `#1e1f22`, center stage placeholder
on `#2b2d31`. The center label (`Stage – 0 sharing, 0 online, 0 in voice, 5
messages`) confirms the session connected and the workspace received the chat
history through the new root→`Workspace` event fan-out. Login is now its own
relm4 component (`ui/login.rs`); the root routes Login → Connecting → Workspace
through a `Stack`. No startup `GtkStack` warning.

