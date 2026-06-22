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

### M6 T6 – members, channels, self-panel; join group voice (2026-06-22)

Two instances (`HEARTH_TITLE=alice|bob`). Both start under the right-rail
**ONLINE** group (each sees the other plus `… (you)`); the left rail shows the
`# general` text channel and a `🔊 Voice (join)` button with the self-panel
(name + **Mute / Deafen / Share screen**) pinned to the bottom. **Each clicks
Voice → both move to the IN VOICE group** (members rail + channel sub-list), the
button flips to `Voice (leave)`, and the **voice mesh connects** – bob's log
prints `incoming voice linked -> playing`. The smaller-UUID offerer rule yields
exactly one offer per pair, so no glare. New relm4 components:
`ui/{members,channels,self_panel}.rs` (members via `FactoryVecDeque`); engine
gained `Session::self_id`/`self_name` (decoded from the JWT) for the panel + the
`(you)` marker.

### M6 T7 – stage + chat + multi-sharer switcher (2026-06-22)

New `ui/stage.rs` (a `gtk::Picture` + a **Watching** switcher, one button per
remote sharer) and `ui/chat.rs` (`FactoryVecDeque` messages + entry). The root
owns the received-paintable map (`Rc<RefCell<…>>`, shared with the workspace) so
the non-`Send` `Paintable` never rides a relm4 message; selecting a tab swaps the
shown paintable instantly. Chat history + live messages render in the centre
panel below the stage; the stage hides (chat fills the centre) when nobody
shares. You never watch your own share (self is excluded from the switcher).

**Observed** (two instances, voice joined, both **Share screen**): **bob's stage
renders alice's live screenshare** – a frame with a ticking `timeoverlay`
timestamp (`0:01:57.314`), proving capture → HEVC → WebRTC → decode →
`gtk4paintablesink` → stage `gtk::Picture`, with the **Watching: alice** tab,
chat, and the voice mesh all concurrent.

**Testing note (do NOT grab the real screen):** run the desktop with
`HEARTH_CAPTURE="videotestsrc is-live=true pattern=ball ! timeoverlay !
videoconvert"` so screenshare streams a synthetic pattern instead of
`ximagesrc`. Grabbing the live X display (`:0`) plus HW-encoding it is heavy and
was observed to destabilise other apps on the same display (e.g. VS Code); the
synthetic source avoids that entirely and makes the stream visibly verifiable.

**Known limitation:** signaling `Offer/Answer/Ice` carry only `(peer, flow)`, and
the engine keys flows by `(peer, Flow)`. Two peers who **share to each other at
the same time** therefore collide on `(other, Screen)` (the outgoing offerer vs.
the incoming answerer), so only one direction connects – seen above as bob
receiving alice while alice does not receive bob. The designed multi-sharer case
(several members share, others watch – distinct peer pairs, e.g. a third viewer
watching two sharers) is unaffected and shows multiple Watching tabs. Supporting
mutual same-pair screenshare needs a per-stream id in the protocol, deferred
alongside the screenshare SFU.

