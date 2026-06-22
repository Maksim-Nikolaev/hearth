# Hearth M7.2 – Screenshare picker freeze fix + live previews + leak teardown

> **For agentic workers:** Use superpowers:subagent-driven-development. Steps use `- [ ]`.

**Goal:** Eliminate the picker UI freeze and the whole-PC freeze, give the selected source a live preview with auto-refreshing grid thumbnails, add a custom bitrate entry, disambiguate audio sources, and tear down a viewer's stale black share when the sharer disconnects.

**Architecture:** The freezes come from synchronous, blocking, real-screen `ximagesrc` captures: the picker calls `engine::screen::thumbnail::thumbnail()` (blocks up to 3 s) once per window **on the GTK main thread**, and `app.rs` restarts the live preview `ximagesrc` pipeline on **every** `ConfigChanged` (including bitrate). Fix = move thumbnail capture to a single background worker thread that delivers frames back via the component input sender, and only restart the preview when the `ShareSource` actually changes. The leaked black share = the viewer never tears down the screen flow on `PeerLeft`.

**Tech:** Rust, GTK4/relm4 (`SimpleComponent`, `ComponentSender::input` is `Send`), GStreamer, Axum backend, `protocol` crate.

## Global Constraints
- Work on `main`, one commit per task, **do not push**, no `Co-Authored-By`. Source Rust: `. "$HOME/.cargo/env"`.
- `cargo build -p engine`, `-p desktop`, `-p backend` compile with **0 warnings**; `cargo test` passes. Do NOT regress M7/M7.1, the queue/black-screen fix, the screen-audio hardening, or the bitrate/encoder work.
- **Never block the GTK main thread on capture.** Never run more than ONE real-screen capture pipeline concurrently (one background worker + the single live preview).
- UI is run-and-observe (human re-tests live). For any in-repo automated check use `HEARTH_CAPTURE="videotestsrc is-live=true pattern=ball ! timeoverlay ! videoconvert"`; never grab the real `:0` screen in automated runs.
- Reuse the stored-`SignalHandlerId` block-during-populate pattern already in `screenshare_picker.rs`.

---

## Task 1: Async, single-worker, auto-refreshing grid thumbnails (fix UI freeze)

**Files:** `desktop/src/ui/screenshare_picker.rs`. Test: none (UI); verify by build + run-and-observe.

**Problem:** `thumbnail()` is called synchronously in a loop on the main thread (init grid build at ~317-328 and rebuild at ~753-764), each blocking up to 3 s → "desktop is not responding".

**Design:**
- `build_source_card` must return both the card `gtk::Box` AND its inner `gtk::Picture`. Store `thumb_pics: Vec<gtk::Picture>` on the model (index-aligned with the source list, index 0 = Whole screen) alongside `source_cards`.
- Build cards immediately with a placeholder (no capture on the main thread). Capture happens off-thread.
- Add `PickerInput::ThumbnailReady { index: usize, png: Vec<u8> }`. On receive, build `gdk::Texture::from_bytes(&glib::Bytes::from(&png))` and `thumb_pics[index].set_paintable(Some(&texture))` (ignore on decode failure).
- Spawn ONE background worker thread (`std::thread::spawn`) that owns the list of `(index, ShareSource)` and loops: for each source sequentially call `engine::screen::thumbnail::thumbnail(&src, 240, 135)`, and on `Some(png)` call `sender.input(PickerInput::ThumbnailReady { index, png })` (clone the `ComponentSender` into the thread — `.input()` is `Send`). After a full pass, `std::thread::sleep(Duration::from_secs(4))` then repeat (auto-refresh). Sequential = never more than one capture at once.
- **Stop signal:** share an `Arc<AtomicBool> running`. The worker checks it each iteration and exits when `false`. Set it `false` (and replace with a fresh flag on next open) when the picker closes — i.e. in the Go Live, Cancel, and any hide path. Store the flag + a `JoinHandle`/just-detach on the model. On a new `Populate`/open, stop the old worker before starting a new one (avoid stacking workers).

- [ ] **Step 1:** Refactor `build_source_card` to also yield the `gtk::Picture`; add `thumb_pics: Vec<gtk::Picture>` and `thumb_worker: Arc<AtomicBool>` to the model; build cards with placeholders only (remove the synchronous `thumbnail()` calls from the main-thread grid build).
- [ ] **Step 2:** Add `PickerInput::ThumbnailReady { index, png }` + handler that sets the texture.
- [ ] **Step 3:** Add a `spawn_thumb_worker(&self, sender, sources)` that stops any prior worker (flip old `AtomicBool`), creates a fresh flag, and spawns the sequential-capture-with-4s-refresh loop. Call it after building the grid (init) and after `Populate`/window-list rebuild. Stop the worker on Go Live and Cancel.
- [ ] **Step 4:** `cargo build -p desktop` 0 warnings. Run-and-observe (synthetic): picker opens instantly (no freeze), cards fill in shortly after, refresh over time.
- [ ] **Step 5: Commit** `git add desktop/src && git commit -m "fix(desktop): async single-worker auto-refreshing screenshare thumbnails (no main-thread freeze)"`

---

## Task 2: Restart the preview only when the source changes (fix whole-PC freeze)

**Files:** `desktop/src/app.rs` (~381-384, the `PickerOutput::ConfigChanged` arm).

**Problem:** every `ConfigChanged` does `s.stop_preview(); s.start_preview(cfg)`, restarting the real `ximagesrc` preview even for bitrate/fps/content changes → rapid real-capture churn freezes the desktop.

**Design:** Track the currently-previewed `ShareSource` on the app model (`previewed_source: Option<ShareSource>`). In the `ConfigChanged(cfg)` handler, only `stop_preview()/start_preview(cfg)` when `Some(&cfg.source) != previewed_source.as_ref()`; otherwise do nothing (the live preview is unaffected by bitrate/fps/quality). Update `previewed_source` whenever a preview starts (here and at the initial `start_preview` on picker-open ~218) and clear it on `stop_preview` paths (Cancel/Go Live/leave). `ShareSource` must be `PartialEq` (derive it in `engine/src/screen/sources.rs` if not already).

- [ ] **Step 1:** Ensure `ShareSource` derives `PartialEq` (and `Eq`) in `engine/src/screen/sources.rs`; build `-p engine`.
- [ ] **Step 2:** Add `previewed_source: Option<ShareSource>` to the app model; set it on every `start_preview`, clear on every `stop_preview`.
- [ ] **Step 3:** Gate the `ConfigChanged` preview restart on a source change only.
- [ ] **Step 4:** `cargo build -p desktop` + `-p engine` 0 warnings. Run-and-observe (synthetic): changing bitrate/fps/quality does NOT restart the preview (no flicker, no churn); switching source DOES.
- [ ] **Step 5: Commit** `git add desktop/src engine/src && git commit -m "fix(desktop): restart screenshare preview only on source change (stop capture churn freeze)"`

---

## Task 3: Custom bitrate entry

**Files:** `desktop/src/ui/screenshare_picker.rs`, `desktop/src/config.rs` (already has `share_bitrate_kbps: u32`).

**Design:** Next to the existing bitrate preset dropdown add a `gtk::SpinButton` (range 500..=50000 kbps, step 250, page 1000) bound to `self.bitrate_kbps`. Changing the spin sets `bitrate_kbps` and emits `ConfigChanged` (block its handler during programmatic `set_value` using the stored-handler-id pattern). The preset dropdown and the spin stay in sync: selecting a preset sets the spin value; typing a custom value leaves the dropdown showing the nearest/"Custom". Persist via the existing `share_bitrate_kbps` Go-Live path. `current_config()` already reads `self.bitrate_kbps`.

- [ ] **Step 1:** Add the `SpinButton` to the Bitrate row; store its `SignalHandlerId`; default its value from saved `share_bitrate_kbps`.
- [ ] **Step 2:** Wire spin→`bitrate_kbps`→`ConfigChanged`; keep preset/spin in sync both ways (block signals on programmatic set).
- [ ] **Step 3:** `cargo build -p desktop` + `cargo test -p desktop` 0 warnings/green. Run-and-observe: type a custom bitrate, Go Live uses it, value persists.
- [ ] **Step 4: Commit** `git add desktop/src && git commit -m "feat(desktop): custom bitrate entry in the screenshare picker"`

---

## Task 4: Disambiguate audio sources (3 "Firefox")

**Files:** `engine/src/screen/audio.rs` (extend `AudioNode` + `parse_nodes`/labels; extend tests), `desktop/src/ui/screenshare_picker.rs` (display the richer label).

**Problem:** multiple streams from the same app all show the bare `application.name` ("Firefox"), indistinguishable.

**Design:** In `parse_nodes`, build a human label that combines `application.name` with a distinguishing field — prefer `media.name` (often the tab/stream title, e.g. "lofi hip hop radio…"), else `node.description`, else the node serial/id. Label format: `"{app} – {detail}"` when a distinct detail exists, else just `{app}`. Keep excluding our own pid, devices, inputs, virtual nodes. Add a unit test feeding a `pw-dump` snapshot with TWO `Stream/Output/Audio` nodes both `application.name="Firefox"` but different `media.name`, asserting two distinct labels.

- [ ] **Step 1:** Failing test (two Firefox streams → two distinct labels).
- [ ] **Step 2:** Extend `parse_nodes`/`AudioNode` label logic; test → PASS; `cargo test -p engine` green.
- [ ] **Step 3:** Picker shows the richer label in the Audio Source dropdown (no logic change if it already renders `node.label`). Build `-p desktop` 0 warnings.
- [ ] **Step 4: Commit** `git add engine/src desktop/src && git commit -m "feat(engine): disambiguate per-app audio sources with stream detail"`

---

## Task 5: Tear down the viewer's stale black share when the sharer disconnects

**Files:** `backend/src/presence/ws.rs` + `backend/src/presence/*` (verify `disconnect(id)` broadcasts `PeerLeft` to the room — add it if missing), `desktop/src/app.rs` (handle `PeerLeft`/screen connection-state to drop the screen-view flow + clear the stage).

**Problem:** when the sharer process dies, the viewer keeps rendering the dead WebRTC screen flow (black). The member-list `PeerLeft` path exists but does not stop the screen view.

**Design:**
- Backend: confirm `signaling.disconnect(id)` removes the peer from the room AND broadcasts `ServerMessage::PeerLeft { user: id }` (and `VoiceLeft` if in voice). If it doesn't broadcast `PeerLeft`, make it.
- Desktop: on `SessionEvent::PeerLeft { user }` (app.rs ~326) AND on the screen `webrtcbin` `connection-state` going `Failed`/`Disconnected`/`Closed`, tear down that peer's screen-view `FlowPeer` and clear the stage (show "no one is sharing" instead of the frozen last frame). Ensure the engine exposes a way to stop/drop a screen-view flow for a given peer (reuse existing teardown; add a `Session` method if needed). The stage must visibly clear, not hold the last black frame.

- [ ] **Step 1:** Read `backend/src/presence/{ws.rs,signaling/*}`; confirm/add `PeerLeft` broadcast on `disconnect`. `cargo test -p backend` green.
- [ ] **Step 2:** Desktop: on `PeerLeft` and on screen `connection-state` failure, drop the screen-view flow for that peer and clear the stage. Add the engine teardown hook if missing.
- [ ] **Step 3:** `cargo build -p backend -p desktop -p engine` 0 warnings. Run-and-observe: sharer killed → viewer's stage clears (no permanent black).
- [ ] **Step 4: Commit** `git add backend/src desktop/src engine/src && git commit -m "fix: tear down viewer screenshare when the sharer disconnects (no stale black)"`

---

## Self-Review
- Covers all reported items: UI freeze (T1), PC freeze on bitrate (T2), custom bitrate (T3), audio disambiguation (T4), leaked black share (T5), live selected-source preview + auto-refresh grid (T1 worker + T2 keeps the live preview stable).
- Largest risk: T1 (threading/refresh lifecycle — must stop the worker on close to avoid leaks/stacked workers) and T5 (cross-subsystem). T2 is the highest-value, lowest-risk freeze fix; consider it the priority.
