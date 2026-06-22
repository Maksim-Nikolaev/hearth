# Hearth M7.1 – Screenshare & Settings Polish Plan

> Follow-up to M7, from live-test feedback. Subagent-driven. Steps use `- [ ]`.

**Goal:** Discord-style polish for the Voice settings page and Screen Share picker, plus fix the empty app-audio list.

**Tech:** Rust, GTK4 + relm4, the M7 engine `Session`/`screen`/`audio` API. `desktop/src/ui/{settings,meter,screenshare_picker}.rs`, `desktop/src/config.rs`, `engine/src/screen/audio.rs`, `engine/src/screen/sources.rs`.

## Global Constraints
- Work on `main`, commit locally, one commit per task. Do **not** push. No `Co-Authored-By`.
- Source Rust with `. "$HOME/.cargo/env"`.
- `cargo build -p engine` and `cargo build -p desktop` compile with **0 warnings**; `cargo test` passes. **Do not regress M7** (voice, settings device/mic-test/DSP, the working share path).
- UI is run-and-observe (the human re-tests live). Screenshare testing uses `HEARTH_CAPTURE="videotestsrc is-live=true pattern=ball ! timeoverlay ! videoconvert"` — never grab the real `:0` screen in automated tests.
- Reuse the existing block-signal-during-populate pattern in `settings.rs`/`screenshare_picker.rs` for any programmatic widget update.

---

## Task 1: Investigate + fix the empty app-audio list

**Files:** `engine/src/screen/audio.rs` (Test: extend its unit tests).

**Problem:** the Screen Share picker's Audio Source dropdown only shows None / Entire System; per-app nodes from `list_app_nodes()` never appear.

- [ ] **Step 1: Reproduce/diagnose.** Run `pw-dump` while an app plays audio (e.g. `mpv`/a browser tab) and inspect the JSON: confirm which `media.class` audio-producing app streams have (`Stream/Output/Audio`) and what identifying props they carry (`application.name`, `media.name`, `node.name`). Compare with what `parse_nodes`/`list_app_nodes_inner` currently extract. Write findings as a comment in the report.
- [ ] **Step 2: Failing test.** Add a unit test feeding a representative `pw-dump` snapshot containing a real app `Stream/Output/Audio` node (with `application.name`) and assert `parse_nodes` returns it with a human label (prefer `application.name`/`media.name` over `node.name`). Run → it should fail if the current filter/label logic misses it.
- [ ] **Step 3: Fix** `keep_node`/`parse_nodes`/`list_app_nodes_inner` so genuine app output streams are returned with a friendly label, while still excluding our own pid, devices, inputs, and virtual nodes. Run the test → PASS; full `cargo test -p engine` → PASS.
- [ ] **Step 4: Commit** `git add engine/src/screen/audio.rs && git commit -m "fix(engine): list app audio output nodes for screenshare audio source"`

---

## Task 2: "Reset to default" button (Voice settings)

**Files:** `desktop/src/ui/settings.rs`, `desktop/src/app.rs` (apply path), maybe `desktop/src/config.rs`.

- [ ] **Step 1:** Add a "Reset to Default" `gtk::Button` to the Voice settings page (near the bottom).
- [ ] **Step 2:** On click, emit `SettingsOutput::ResetDefaults`. The root (`app.rs`) sets `Settings::default()`, persists via `Config::save_settings`, applies to the live `Session` (the existing `apply_settings_to_session` path), and sends `SettingsInput::SetSettings(Settings::default())` so the window repopulates — reusing the existing block-signal populate so it emits no spurious outputs.
- [ ] **Step 3:** Build (0 warnings) + `cargo test -p desktop`. Run-and-observe: open Settings, change toggles, Reset → controls snap back to defaults and persist.
- [ ] **Step 4: Commit** `git add desktop/src && git commit -m "feat(desktop): reset-to-default button in voice settings"`

---

## Task 3: Clamp resolution presets to the native display size

**Files:** `desktop/src/ui/screenshare_picker.rs`; a small helper (gdk monitor geometry).

**Decision:** HIDE presets larger than the display's native resolution (e.g. on 2560×1440, show 480p/720p/1080p/1440p, hide 4K).

- [ ] **Step 1:** Determine the native resolution from GTK: `gtk::gdk::Display::default()` → the primary/first `Monitor`'s `geometry()` (width×height, times `scale_factor` if needed). Store as `(native_w, native_h)`.
- [ ] **Step 2:** When building the resolution preset buttons (`res_presets`), only append a preset whose `(w,h)` is `<= (native_w, native_h)`. Always keep at least the largest preset that fits; if the saved/default selection was hidden, fall back to the largest visible preset.
- [ ] **Step 3:** Build (0 warnings). Run-and-observe: on a 1440p display the picker shows up to 1440p, no 4K.
- [ ] **Step 4: Commit** `git add desktop/src && git commit -m "feat(desktop): clamp screenshare resolution presets to native display"`

---

## Task 4: "You're sharing" indicator

**Files:** `desktop/src/ui/self_panel.rs`, `desktop/src/ui/workspace.rs`, `desktop/src/ui/members.rs` (and/or `theme.rs`), `desktop/src/app.rs`.

**Goal:** when YOU are sharing, make it obvious (beyond the subtle Share button).

- [ ] **Step 1:** Track local-share state in the workspace (it already routes StartShare/StopShare). Add `SelfPanelInput::SetSharing(bool)`.
- [ ] **Step 2:** When sharing: restyle the Share button to an active/red "● Sharing — click to stop" state (CSS class in `theme.rs`, e.g. `.sharing` with the accent/red colour) and/or add a small "🔴 LIVE" label in the self-panel. Also tag your own row in the members **In Voice** list with a 🔴/"LIVE" marker (the members list already renders rows — pass a `sharing` flag for self).
- [ ] **Step 3:** Wire StartShare/GoLive → SetSharing(true); StopShare/Cancel/leave → SetSharing(false). Block the toggle handler during any programmatic set (reuse the stored handler-id pattern).
- [ ] **Step 4:** Build (0 warnings). Run-and-observe: going live shows the indicator; stopping clears it.
- [ ] **Step 5: Commit** `git add desktop/src && git commit -m "feat(desktop): clear 'you are sharing' live indicator"`

---

## Task 5: Combined input-sensitivity + live-level bar (Discord-style)

**Files:** `desktop/src/ui/settings.rs`, `desktop/src/ui/meter.rs` (merge/replace), maybe `theme.rs`.

**Goal:** one horizontal bar where the **live mic level** fills the track and the **sensitivity threshold** is a draggable handle; the portion below threshold is orange, above is green (matches the reference image).

- [ ] **Step 1:** Replace the separate Meter + Input-Sensitivity Scale with ONE custom widget: a `gtk::DrawingArea` (or `gtk::Overlay` of a `LevelBar` behind a `Scale`) that draws (a) the track, (b) a filled bar = current input level (dBFS → 0..1), (c) a handle at the threshold position. Below-threshold portion orange (`#f0a500`-ish), above-threshold green (`#3ba55d`-ish), matching Discord.
- [ ] **Step 2:** Input: `SetLevel(f32 dBFS)` updates the live fill; dragging the handle (pointer/`GestureDrag` on the DrawingArea, or the Scale value) sets `input_sensitivity` and emits `SettingsOutput::InputSensitivity(f32)`. Keep the meter live both in Mic Test and in a call.
- [ ] **Step 3:** Remove the now-redundant standalone meter+slider; keep the engine wiring (`InputLevel` → this widget; threshold → `Session::set_activation(Voice{threshold})`).
- [ ] **Step 4:** Build (0 warnings) + `cargo test -p desktop`. Run-and-observe: speaking fills the bar; the colour split sits at the handle; dragging changes the VAD threshold.
- [ ] **Step 5: Commit** `git add desktop/src && git commit -m "feat(desktop): Discord-style combined sensitivity + level bar"`

---

## Task 6: Discord-style source grid with thumbnails (Screen Share picker)

**Files:** `desktop/src/ui/screenshare_picker.rs`; possibly a small engine helper for a one-shot thumbnail.

**Goal:** replace the plain row of text source buttons with a **grid of cards**, each a thumbnail + title (reference images).

- [ ] **Step 1:** Lay the source list out as a `gtk::FlowBox` (or `Grid`) of cards: each card = a `gtk::Picture` thumbnail above a title label, in a styled rounded box (CSS in `theme.rs`); selected card highlighted. "Whole screen" is the first card; one card per `list_windows()` window.
- [ ] **Step 2: Thumbnails.** Add an engine helper `screen::thumbnail(source, max_w, max_h) -> Option<gdk::Paintable>` (or PNG bytes) that captures a SINGLE frame of the source (`ximagesrc [xid] num-buffers=1 ! videoconvert ! videoscale ! caps(small) ! gtk4paintablesink`/`pngenc`) — light, one-shot, not a live pipeline. Under `HEARTH_CAPTURE`, the thumbnail uses the synthetic frame. Populate each card's `Picture`; if a thumbnail fails, show a placeholder.
- [ ] **Step 3:** Selecting a card updates the picker's `ShareSource` and the big live preview (existing `start_preview`); keep the Stream-Settings (resolution/fps/content/audio) + Go Live/Cancel below.
- [ ] **Step 4:** Build (0 warnings). Run-and-observe (synthetic): the picker shows a thumbnail grid; selecting a source updates the preview; Go Live still works.
- [ ] **Step 5: Commit** `git add desktop/src engine/src && git commit -m "feat(desktop): Discord-style screenshare source grid with thumbnails"`

---

## Self-Review
- Covers all six live-test items: app-audio list (T1), reset (T2), resolution clamp/hide (T3), sharing indicator (T4), combined sensitivity bar (T5), thumbnail grid (T6). The two black-screen bugs were already fixed (`3e2d062`, queue in `capture_chain`).
- Risk: T5 (custom drawing) and T6 (per-source thumbnails) are the largest; thumbnails are one-shot single frames to stay light. T1 may be a quick filter fix once `pw-dump` is inspected.
