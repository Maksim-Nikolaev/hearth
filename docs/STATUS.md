# Hearth – Status

_Living status doc. Last updated: 2026-06-25._

Self-hosted, low-latency voice + high-fidelity screenshare + webcam for 2–3 close
friends over a P2P mesh with a small Rust control server. Stack: **pure-Rust
GTK4 + relm4 desktop client** calling the Rust media engine (GStreamer
`webrtcbin`) directly – no language bridge; Rust/Axum backend. A Flutter mobile
app is a later, separate effort (shares only backend + protocol, uses
`flutter_webrtc`). Work happens on `main`, committed locally, never pushed unless
asked.

**Media transport: per-flow PeerConnections** – one `webrtcbin` per flow (Voice /
Screenshare+audio / Webcam), chat over the WebSocket. Each flow drops and
congests independently (revised from the earlier single-bundle "Approach A").

## Milestones

| Milestone | State | Notes |
|-----------|-------|-------|
| M0–M1 backend foundation | ✅ done, green | Axum, PG18, users, argon2, JWT, `/auth/*`, admin `/users`, WS `/ws` presence |
| M2 media spike | ✅ GO (loopback) | throwaway `engine-spike/`; `vah265enc` HW HEVC; two-peer `webrtcbin` over `/tmp` |
| M3 signaling | ✅ done, green | `backend/src/signaling/` relays join/offer/answer/ice/leave over `/ws` |
| M4 networked engine (Tasks 1–4) | ✅ done, Linux loopback GO | real `engine/` crate driven by the server, no `/tmp` |
| M4 Task 5 (cross-machine) | ⏸ blocked – user-run on Windows | the Approach-A go/no-go; needs the Windows 11 box |
| M5 Rust GTK4 desktop shell | ✅ done, verified | relm4 app: login + presence + chat + voice (engine) + screenshare share/view **in-window**; per-flow framework. 9 tasks, all committed on `main` |
| M6 Discord group experience | ✅ done, verified | 3-pane shell (channels+self-panel \| stage+chat \| members); group **voice mesh** (smaller-UUID offerer); multi-sharer screenshare + Watching switcher behind a `ScreenTransport` seam. 7 tasks on `main`. Two-instance live run: voice mesh + bob renders alice's screenshare on the stage. |
| M7 voice processing + advanced screenshare | ✅ implemented; voice live-verified, UI verification pending | Audio device enumeration; in-process voice DSP (`webrtc-audio-processing` crate, bundled build); activation gate (mute > ptt > vad > always); single mic-capture → DSP → fan-out pipeline with speaker-monitor AEC reference; standalone mic-test monitor; X11 global PTT (`XGrabKey`); screenshare source/quality/preview + app/system audio via PipeWire; desktop Settings model + Voice settings page + Screen Share picker. 12 tasks on `main`. Group voice live-verified two-instance both directions (see verification log). Settings UI + share picker built and unit-tested but **live-verification pending (human)**. |

## Component state

**Backend (`backend/`)** – Axum + sqlx + PG18 (dev container, host port 5433).
Auth (JWT + argon2, admin-provisioned, revocable refresh), presence + signaling
multiplexed over `/ws`. 15 tests green. Signaling message types now come from the
shared `hearth-protocol` crate.

**Protocol (`protocol/`)** – `hearth-protocol`: `PeerInfo` / `ClientMessage` /
`ServerMessage`, both directions `Serialize + Deserialize`. Backend re-exports it;
engine depends on it directly. Single source of truth for the wire format.

**Engine (`engine/`)** – product crate (supersedes `engine-spike/`):
- `encoders` – runtime HW HEVC probe (selects `vah265enc` on this box).
- `capture` – per-OS chain; `HEARTH_CAPTURE` override; `videorate`/`videoscale`/caps.
- `signaling` – REST login → JWT WebSocket, typed send/recv (mock-WS unit test).
- `peer` – `webrtcbin` driven by the signaling client; `share`/`view` modes.
- CLI: `engine probe|share|view`, fully env-configured. Bus error/warning watch.
- **Verified:** Linux loopback through the real backend – both peers `Connected`,
  viewer linked + displayed the stream. **One video track (screenshare) only.**

## Verified vs. open

**Verified (Linux):** signaling-driven offer/answer/ICE, HW HEVC encode/decode,
single screenshare track end-to-end through the server, no-recompile env config.

**Open / not yet built:**
- **Per-flow media** – voice + webcam flows (each its own `webrtcbin`); only the
  screenshare flow exists today. M4's screenshare peer IS that flow.
- **Multi-peer mesh** – currently 1:1 (first peer). Spec wants 2–3 friends.
- **Cross-machine / cross-NAT** – latency, AMF encode, direct-ICE-vs-TURN
  (Task 5, Windows-blocked). Runbook: `engine/docs/windows-setup.md`.
- **App** – no Flutter UI yet; engine is CLI-only.
- **Backend features** – text-chat persistence, attachments (RustFS two-phase
  upload per the Lezio pattern) not started.

## M5 done (2026-06-22)

Pure-Rust desktop client (`desktop/`, GTK4 + relm4) calling `engine` directly, in
a root Cargo workspace (`protocol`, `engine`, `backend`, `desktop`). Verified with
two live instances against the backend: login (token via `keyring`), presence,
text chat (+ backend `messages` slice), and **screenshare displayed in-window**
via `gtk4paintablesink` → `gtk::Picture`. Engine refactored to a library API
(`Session` / `Flow` / `FlowPeer`, per-flow `webrtcbin`s, `SessionEvent`); voice
flow loopback-verified at the engine level; mute/deafen + Stop wired. CLI
`probe/share/view/call/listen` still work, so M4 Task 5 is unaffected. Spec +
plan: `docs/superpowers/{specs,plans}/2026-06-21-hearth-m5-desktop-shell*`.

Runtime note: in-window video needs `gtk4paintablesink` from `gst-plugins-rs`,
built locally and installed to `~/.local/lib/hearth-gst-plugins/`; the desktop
app prepends it to `GST_PLUGIN_PATH` automatically.

## M6 done (2026-06-22)

Discord-style group experience on the M5 shell. Protocol gained voice/share
messages; the backend gained a **voice sub-room** (membership roster + join/leave
notify + share relay, integration-tested in `tests/voice.rs`). The engine
`Session` decodes `self_id`/`self_name` from the JWT, builds a **group voice
mesh** (one Voice `FlowPeer` per member, smaller-`Uuid` peer offers –
`should_offer`), and fans screenshare to each voice member behind a
**`ScreenTransport`** seam (`P2pTransport` now, SFU later). The desktop monolith
split into relm4 components: `app.rs` (root: `Session` + routing) +
`ui/{login,workspace,channels,self_panel,stage,chat,members}.rs` + `theme.rs`
(dark CSS). 7 tasks, one commit each, on `main`. TDD for protocol/backend/engine;
UI run-and-observe (see `desktop/README.md` M6 T5–T7).

**Verified (two live instances, synthetic capture):** dark 3-pane shell; presence
+ chat; **join Voice → group voice mesh connects**; **Share → the viewer's stage
renders the sharer's screen** (live `timeoverlay`) under a **Watching** switcher,
with chat and voice running concurrently.

**Known limitation (screenshare, same as the SFU gap):** `Offer/Answer/Ice` carry
only `(peer, flow)` and the engine keys flows by `(peer, Flow)`, so two peers
**sharing to each other simultaneously** collide on `(other, Screen)` and only one
direction connects. The designed multi-sharer case (several share, others watch –
distinct peer pairs) is unaffected. A per-stream id in the protocol fixes it;
deferred alongside the screenshare SFU.

## M7 done (2026-06-23)

Voice processing, device selection, and advanced screenshare. All 12 tasks committed on `main`; engine tasks TDD, desktop tasks run-and-observe. No backend or protocol changes.

**Engine additions:**

- `engine::audio::devices` – `DeviceMonitor`-based enumeration of audio sources and sinks; `pulsesrc`/`pulsesink` replacing `autoaudiosrc`/`autoaudiosink` with a selectable `device=` (falls back to default).
- `engine::audio::dsp` – wraps the `webrtc-audio-processing` Rust crate (bundled build via autotools): AEC / NS-level / AGC / VAD / high-pass, processing 10 ms interleaved i16 frames at 48 kHz; config applied live.
- `engine::audio::capture` – single mic-capture → DSP bridge → fan-out send branch (`appsink` PCM → `dsp.process_capture_frame()` → `appsrc`); speaker-monitor `appsink` tap feeds `dsp.process_render_frame()` as the AEC reference; `level` element drives the sensitivity meter.
- `engine::audio::monitor` – standalone mic-test pipeline (capture → DSP → level → playback) for testing the mic without being in a call.
- `engine::hotkey` – X11 global push-to-talk via `XGrabKey` (x11rb/XCB); in-app GTK handler as fallback.
- `engine::screen::sources` – X11 `_NET_CLIENT_LIST` window enumeration (id, title, thumbnail) + per-monitor entries; `ximagesrc` for whole screen or `ximagesrc xid=<win>` for a window.
- `engine::screen::capture` – resolution / fps / content-type quality controls; `tee` for local preview via `gtk4paintablesink`.
- `engine::screen::audio` – PipeWire node listing with venmic-style filters (exclude own pid, virtual/loopback nodes); builds a stereo 48 kHz DSP-off Opus audio track for the Screen flow from `pipewiresrc` (specific app) or `pulsesrc <sink>.monitor` (entire system).
- Activation gate: precedence mute > push-to-talk > VAD > always-on; all drive the existing `mic_valve`.

**Desktop additions:**

- `config.rs Settings` model – device pickers, NS/AEC/AGC/VAD flags, sensitivity, activation mode, PTT key, share resolution/fps/content-type/audio-source; persisted to local TOML/JSON config.
- `ui/settings.rs` – Settings window with Voice page: device dropdowns, Mic Test + level meter, NS/AEC/AGC/VAD toggles, sensitivity slider, activation mode selector, PTT keybind capture.
- `ui/screenshare_picker.rs` – source grid (screens + windows, thumbnails), full-res live preview, resolution/fps/content-type rows, audio-source dropdown (None / Entire System / per-app list), Go Live button.
- `ui/meter.rs` – reusable `gtk::DrawingArea` level meter.

**Verified live (2026-06-23):** group voice connects both directions and is audible through the new single-capture DSP path (two instances, Opus 64 kbps fullband confirmed in encoder log). Perceived lower quality is the expected effect of DSP defaults (NS/AGC/AEC all on); the AEC monitor-reference cancels your own replayed voice when both instances run on the same machine (a single-machine test artifact, not a regression in normal use).

**Live-verification pending (human):** Voice settings UI (device switch, Mic Test meter, DSP toggles, PTT key) and Screen Share picker (source/quality/audio-source/preview, screenshare app/system audio).

**Known limitations / follow-ups:**

- Mic/speaker volume sliders persist to config but are not applied to the live session (no engine volume setter yet).
- `ShareAudio::App` choice is not persisted across sessions.
- `webrtcdsp` GStreamer element unavailable on Ubuntu Noble – hence the in-process crate approach.
- DSP defaults (NS/AGC/AEC on) can sound heavily processed; per-user tuning is the intended path.
- The M6 mutual same-pair screenshare limitation still stands (deferred with the screenshare SFU).

## Voice native rewrite — Phase 1 + Phase 2 (2026-06-24, Windows `windows-phase2-audio`)

The voice path was rebuilt for state-of-the-art latency, **off webrtcbin/GStreamer
DSP**. Live-verified on the Win11 box at **~50–55 ms** (at the OS loopback floor),
two-instance both directions.

- **Transport (Phase 1):** voice left `webrtcbin` for **raw RTP/Opus over UDP** —
  Opus restricted-lowdelay, 5 ms frames, in-band FEC + PLC (decode-None on gap),
  `[seq:u16 BE | payload]`, late/dup drop. Self-allocated recv port.
- **Native I/O (Phase 2):** `engine::audio::native` — WASAPI **IAudioClient3**
  capture + playback (mixer, ~2-period render-ahead, ~20 ms lane cap) on two tight
  threads. Devices not at 48 kHz fall back to a WASAPI **AUTOCONVERTPCM** resample
  stream (the IAudioClient3 fast path needs the device's own 48 kHz mix rate; a
  Settings hint recommends 48 kHz). `engine::audio::native_voice` runs the whole
  capture chain.
- **DSP suite (pure-Rust where possible, all opt-in, default OFF):** NS =
  `nnnoiseless` (RNNoise, graduated wet/dry); VAD = `earshot` (480-sample 48 kHz);
  AGC = envelope-follower (relaxes to unity in silence); **AEC = `aec-rs`
  (speexdsp)** with a far-end ring tapped from the playback mix — validated on
  speakers (killed the feedback loop). Toggled live via `dsp_config`.
- **Gate:** voice-activity / PTT / always-on; threshold-accurate (hold-gate, no
  hysteresis band); **200 ms hold + asymmetric ramp** (~5 ms attack, ~80 ms
  release) for click-free, continuous speech; mixer uses a **soft limiter** not a
  brick-wall clamp.
- **UX:** Discord-style member statuses (muted/deafened/🔊speaking, via new
  `VoiceUpdate`/`VoicePeerUpdate` protocol + backend relay); working mic test
  (in-call self-monitor through the real chain, transmitting glow, dB meter scale);
  PTT key + mouse-side-button binds; live-applying settings (no Save); device
  hot-swap; all filters default off + Reset-to-Defaults resync.
- **Windows toolchain:** `scripts/dev/win-env.ps1` now adds **cmake + libclang +
  `/D_USE_MATH_DEFINES`** so `aec-rs`/speexdsp builds under MSVC. See
  [[windows-build-env]].
- **Debug:** `eprintln!` traces left in on purpose (`open render/capture`,
  `filters: …`, AUTOCONVERTPCM) — gate behind a verbose flag before "done-done".

GStreamer voice path remains as a fallback (`HEARTH_GSTREAMER_VOICE`). **Screenshare
still on webrtcbin** — the next major workstream (mirror this rewrite: HW-encode →
RTP/UDP, ~20–30 ms).

## Linux/Wayland voice verified (2026-06-24, Kubuntu)

Re-verified the voice channel after the Mint→Kubuntu (X11→Wayland) move. Two
live desktop instances (alice/bob) through the backend: **voice connects and is
audible both ways on Wayland**, clean quality. OBS-measured mouth→system acoustic
**~74 ms with all DSP filters on, but ~7.14 ms with filters OFF**.

- **Path on Linux = GStreamer `voice_udp`** (raw RTP/Opus/UDP), `pulsesrc` capture
  + `webrtc-audio-processing` DSP + `autoaudiosink`. The Phase-2 native I/O
  (WASAPI IAudioClient3) is Windows-only.
- **The DSP is the dominant latency cost, not the device path.** Per-filter
  attribution (full table in `docs/findings/voice-latency-linux.md`): baseline
  filters-off ≈7 ms; **NS and AEC each add ~one 10 ms frame (~7 ms), roughly
  additive; VAD and AGC are free; NS level is latency-free.** Full processing
  ~20 ms — still under the Windows 36–50 ms floor.
- Because the device path is already ≈7 ms, **native PipeWire small-quantum is a
  robustness nicety, not the latency win** (revises the earlier "Pulse-compat
  buffer is the bottleneck" assumption). The real lever with processing on is
  reducing NS/AEC frame cost — a low-delay `webrtc-audio-processing` config, or
  porting the Windows pure-Rust suite (`nnnoiseless` + `aec-rs`) to converge both
  platforms on one DSP.
- **Fixes landed:** (1) the desktop startup banner no longer claims "NATIVE WASAPI"
  on Linux — it now prints the true per-platform backend. (2) In-call **mic-test
  self-monitor on Linux**: `VoiceCapture` gained `set_self_monitor`, feeding the
  post-DSP (pre-gate) mic into a local `autoaudiosink`/`pulsesink` branch, so you
  hear yourself during a call without a second capture (parity with the native
  path). Previously the GStreamer path refused mic-test during a call. **Live
  human verification of the self-monitor still pending.**
- **Build env (Kubuntu):** needs `libgstreamer1.0-dev`, `libgstreamer-plugins-base/
  bad1.0-dev`, `libgtk-4-dev`, `libspeexdsp-dev`, autotools. `aec-rs-sys` only
  builds with the speex include dir on the compiler path — committed in
  `.cargo/config.toml` (`BINDGEN_EXTRA_CLANG_ARGS`/`CFLAGS`/`CXXFLAGS=-I/usr/include/speex`).
  The home migration also left a partially-copied Cargo registry (corrupt `cc`
  crate); fixed by wiping `~/.cargo/registry/src` to force re-extraction.

## Voice DSP profiles landed (2026-06-24, on main)

`VoiceProfile` Custom/Headset/Speaker/Auto above the existing per-platform DSP
engines (approach C: best engine per platform, unified profile layer). Default
**Custom** = today's behavior unchanged. Headset = AEC off (low latency); Speaker
= AEC on; Auto classifies the output device (Linux: form-factor/icon-name; Windows
deferred → Unknown→Headset). Single "custom slot"; editing a toggle on a preset
demotes to Custom (overwrites). Plus an rtkit-aware RT-scheduling warning and a
Re-probe button. 6 TDD tasks + post-verify fixes, all on main (engine 63 + desktop
6 tests). Spec/plan in `docs/superpowers/{specs,plans}/2026-06-24-voice-dsp-profiles*`.

**Known limits:** Auto can't classify a generic USB "analog stereo" headset (no
form factor) → Unknown→Headset, manual selector is the guarantee. Volume sliders
still not wired to the live session (pre-existing). Mic/speaker volume setter TODO.

## Native PipeWire voice landed — audio work complete (2026-06-25, on main)

Linux voice now **defaults to the native path** (not GStreamer): a full
**pipewire-rs** capture/playback backend (`engine/src/audio/native/native_pw.rs`)
behind the same `NativeCapture`/`NativePlayback` API as Windows WASAPI, so the
whole `native_voice.rs` chain (DSP → Opus → UDP) runs on Linux unchanged.
Measured **~6–14 ms** acoustic mouth-to-ear (corr>0.95) — this **supersedes the
earlier "native PipeWire is a robustness nicety, not the latency win"
assumption**: the pinned quantum both fixes the long-session drift (old pulsesrc
crept to ~70 ms) and lands well under the old DSP-on numbers.

- **Best-driver-with-fallback (both platforms):** native by default; native
  construction failure auto-falls-through to the generic GStreamer `voice_udp`
  path for the session; `HEARTH_GSTREAMER_VOICE=1` forces generic. Live backend
  shown in Settings ("Audio engine: Native (PipeWire) / Generic (GStreamer)").
- **Selectable + tuned AEC:** `SpeexAec` (our `aec-rs-sys` wrapper; speex's own
  denoise/AGC off; strength is a wet/dry mix, 0 = off → 100 = full) vs **WebRTC
  AEC3** (`webrtc_aec.rs`); **WebRTC is the default where supported** (Unix-only
  build → Windows native uses Speex, selector hidden there). Strength + method
  apply live; the mic test runs the full chain so AEC is audible in-test.
- **pipewire-rs gotchas fixed:** honor `buffer.requested()` (else lane over-drain
  → garble), request stereo out + duplicate mono (else front-left only), pinned
  `node.latency=256/48000` (env `HEARTH_PW_QUANTUM`), `pipewire` feature
  `v0_3_65`, build dep `libpipewire-0.3-dev`.
- Spec/plan: `docs/superpowers/{specs,plans}/2026-06-24-native-pipewire-voice*`;
  findings + verification in `docs/findings/voice-latency-linux.md`.

**This concludes the Audio workstream.** Voice on Linux + Windows is native,
low-latency, with auto-fallback and user-tunable echo cancellation.

> Note: the OBS two-track latency harness (`scripts/measure/`) is unreliable below
> ~15 ms — per-source buffering swamps the signal and a working AEC decorrelates
> the tracks (negative/low-corr readings). Trust corr>0.95 only; for sub-15 ms use
> same-clock hardware loopback.

## Roadmap — phases (decided 2026-06-25)

Recently landed on `main`: **mic/speaker volume wiring** (live, attenuate-only,
both paths) and the **dockerized always-on backend + Postgres** (`compose.yml`,
`make` targets, sops+age secrets, `seed` subcommand). Everything to date is
verified **localhost / single-machine**; the remaining unknown is cross-machine.

Phases run in order; deferred items are pulled in only when they bite.

### Phase A — Private network *(ACTIVE NEXT; the de-risk)*
A private overlay (**Headscale** self-hosted or **ZeroTier**) so two machines
reach each other; run the dockerized backend on a node; verify **voice + existing
screenshare cross-machine** with the current raw-UDP P2P — **no NAT-traversal
code**. Load-bearing: it validates the whole architecture over a real network
*and* defers public NAT traversal indefinitely (likely the real v1 deployment
model). Effort: small–medium (overlay config + runbook; maybe a minor tweak so
peers advertise their overlay IP to signaling).

### Phase B — Screenshare GPU capture *(the big feature; two parts)*
- **B1 capture (OBS/Vesktop per-source picker):** Wayland (`xdg-desktop-portal`
  ScreenCast + `pipewiresrc` + DMABuf→VA, behind the `CaptureBackend` seam) and
  Windows (WGC/DXGI + `d3d11`). X11 stays CPU `ximagesrc` ≤60 fps. The visible
  win: Wayland support + low CPU + per-window/region/audio selection. See
  `docs/superpowers/plans/2026-06-23-hearth-m8-screenshare-gpu-capture.md` and the
  X11 GPU-capture NO-GO spike.
- **B2 transport (Moonlight *recipe*, not dependency):** screen flow off
  `webrtcbin` → HW-encode + UDP + FEC + HW-decode + frame pacing (mirrors voice's
  Phase-1). After B1. Do **not** adopt Sunshine/Moonlight wholesale — it's
  whole-desktop 1:1 and gives no per-source picker.

### Phase C — Webcam flow
Drop-in video flow into the per-flow framework once B's patterns exist. Low
priority, small.

### Phase D — UI/UX Discord-pass
Continuous light touches anytime; the big redesign **last**, after features
stabilize (so it isn't re-skinning a moving target).

### Deferred / cross-cutting (pull in on-demand)
- **Public NAT traversal** (STUN hole-punch + coturn TURN) — only if the private
  overlay is outgrown. *Deferred by Phase A.*
- **Screenshare SFU + per-stream id** — when >3 viewers, or the mutual same-pair
  share bug (M6 limitation) bites.
- **Multi-peer mesh hardening / 3-way** — naturally exercised in Phase A.
- **Deployment hardening** — Traefik/TLS, coturn, Grafana/Loki; packaging/
  auto-update.
- **App rename** (Hearth → a candidate) — cosmetic, anytime.
- **Chat persistence / attachments** (RustFS two-phase upload) — backend feature.
- **Cross-machine Windows↔Linux measurement** (M4 Task 5) — folds into Phase A.

## How to resume / run

- Source Rust: `. "$HOME/.cargo/env"`. Dev DB: `docker compose -f compose.dev.yml up -d postgres` (host port 5433).
- Backend: `DATABASE_URL=postgres://hearth:hearth@localhost:5433/hearth JWT_SECRET=dev-only-change-me-min-32-bytes-long-secret cargo run` (listens `0.0.0.0:8080`).
- Seed users: `password::hash` + `users::repository::create` (admin endpoint also exists).
- Engine loopback: backend up + two users, then `engine view` and `engine share` (see `engine/README.md`).
- Plans: `docs/superpowers/plans/`. Spec: `docs/superpowers/specs/2026-06-21-hearth-design.md`.
