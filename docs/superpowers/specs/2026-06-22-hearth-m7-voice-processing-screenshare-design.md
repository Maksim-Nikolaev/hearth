# Hearth M7 – Voice Processing, Device Selection & Advanced Screenshare (Design)

_Status: design agreed 2026-06-22. Builds on the M6 group experience._

## Goal

Bring Hearth's voice and screenshare up to Discord/TeamSpeak/Mumble-tier control:
selectable input/output devices, a mic test meter, voice DSP (echo cancellation,
noise suppression, automatic gain control, voice-activity detection),
push-to-talk, and a real Screen Share picker with selectable source (whole
screen / specific window), per-app or system audio, quality (resolution / fps /
content-type) and a live preview. All client-side; no backend or protocol
changes.

## What we run today (baseline)

- Media is **WebRTC via GStreamer `webrtcbin`** (ICE/DTLS-SRTP), signalled over
  the Axum WebSocket. **Not** the Mumble protocol, **not** `webrtc-rs`, **not**
  Sunshine. Voice uses **Opus**; screen uses hardware **HEVC**.
- Voice send chain has **zero processing**:
  `autoaudiosrc ! audioconvert ! audioresample ! opusenc`; recv is
  `opusdec ! … ! autoaudiosink`. `autoaudiosrc/sink` grab the **system default**
  device. No device picker, no DSP, no meter, no activation gate.
- Screenshare is **video-only**, fixed caps in `engine/src/capture.rs`
  (`ximagesrc` on Linux), no source/quality picker, no preview.
- The desktop already has a per-flow engine API (`Session`/`FlowPeer`, one
  `webrtcbin` per flow) and a relm4 component tree from M6.

## Target platform (detected)

X11 session (`DISPLAY=:0`), **PipeWire 1.0.5** running behind the PulseAudio
shim. So: video capture via `ximagesrc` (with `xid=` for a window); audio device
enumeration and **per-application audio capture** available through PipeWire.
The GStreamer `webrtcdsp`/`webrtcechoprobe` elements are **not shipped** by
Ubuntu Noble's plugins-bad (confirmed via `gst-inspect-1.0`), so voice DSP runs
**in-process via the `webrtc-audio-processing` Rust crate** instead. `level`,
`pulsesrc`, and `pipewiresrc` are present; cmake/clang/pkg-config (for the crate
build) are present.

## Locked decisions

- **One combined milestone**, internally decomposed into five independently
  testable units; implementation staged in that order.
- **Voice DSP = the `webrtc-audio-processing` Rust crate, in-process** (libwebrtc
  AudioProcessing 0.3: AEC / NS / AGC / VAD). The GStreamer `webrtcdsp` element is
  **not available on Ubuntu Noble** (plugins-bad is built without it), so instead
  of an in-pipeline element we bridge capture PCM through the crate before Opus.
  Per-toggle control is **live** (set the processor config in place), and the same
  crate **ports to the future Windows build**. Builds via the already-installed
  autotools toolchain (libtool/automake/autoconf).
- **Apply changes live**: changing a device or DSP toggle during a call rebuilds
  the voice send branch / hot-swaps the source (sub-second, no reconnect).
- **Screenshare sources (X11): whole screen + specific window.** Region-select is
  out of scope. True per-app *grouping* is a Wayland/portal feature, not X11.
- **Screenshare audio: None / Entire System / Specific application**, captured
  **directly via PipeWire** (`pipewiresrc target-object=…` for an app,
  `pulsesrc device=<sink>.monitor` for the system) – **no virtual-mic / venmic
  addon needed**. The screenshare audio track is **stereo, 48 kHz, DSP-off**
  music-mode Opus, separate from the voice DSP chain.
- **Activation modes**: voice-activity threshold, **global** push-to-talk (X11
  `XGrabKey`), and always-on. All drive the existing `mic_valve`.
- **Settings model**: flat "Custom" toggle set (skip Discord's named profiles).
  **Local config file** only; no backend/protocol/server-sync.
- **Primary target is Linux (Ubuntu/Mint, X11).** A **Windows** build for less
  technical friends is a planned **later** milestone: the device/capture/DSP
  backends sit behind the engine API, so Windows swaps the platform layer
  (`wasapisrc`/`wasapisink`, `d3d11screencapturesrc` + WASAPI loopback for app
  audio, `RegisterHotKey` for PTT) without touching the UI or signalling. Out of
  scope for M7.

## Insights adopted from Vesktop (reference reading)

Vesktop is Electron + Discord's web client, so it is **not** a reference for
voice DSP (that is Discord/Krisp). The useful parts:

- **App/system audio is PipeWire node patching.** Vesktop's `venmic` builds a
  *virtual mic* only because the browser can capture a microphone and nothing
  else. We have no such limit – GStreamer captures a PipeWire node directly – so
  we **drop the native addon** and keep only venmic's **node-filtering rules**:
  exclude **our own process** (feedback loop), and offer "ignore device nodes /
  ignore input streams / ignore virtual (loopback) nodes / only apps playing to
  (default) speakers" when listing app sources.
- **Screenshare audio = stereo / 48 kHz / no DSP** (Vesktop requests
  `autoGainControl/echoCancellation/noiseSuppression: false`, `channelCount: 2`,
  `sampleRate: 48000`).
- **Picker UX**: small thumbnails in a grid, full-res preview of the selected
  source; PipeWire-absent graceful gate (`hasPipeWire()` warning + override).
- **Settings**: a JSON store with change-listeners and `FormSwitch` / radio rows
  – validates our local-config + toggle/radio UI.

## Architecture – five units

```
engine/src/
  audio/
    devices.rs      # DeviceMonitor enumeration (sources, sinks), hotplug
    dsp.rs          # webrtc-audio-processing crate wrapper (AEC/NS/AGC/VAD)
    capture.rs      # voice capture+DSP-bridge+meter+valve branch (rebuilds live)
    monitor.rs      # standalone mic-test/meter pipeline (no call needed)
  screen/
    sources.rs      # X11 screen + window enumeration (+ thumbnails)
    audio.rs        # PipeWire node listing + filtering (venmic rules)
    capture.rs      # video source + caps (res/fps/content-type) + preview tee
  hotkey.rs         # X11 XGrabKey global push-to-talk
  session.rs        # wires settings -> flows; live-apply; screenshare audio track

desktop/src/
  config.rs         # + Settings (devices, DSP, activation, screenshare defaults)
  ui/
    settings.rs           # Settings window, Voice page
    screenshare_picker.rs # source grid + preview + quality + audio source
    meter.rs              # level meter widget
```

### Unit 1 – Audio I/O & device enumeration (`engine::audio::devices`)

- `list_devices() -> Vec<AudioDevice { id, label, kind: Source|Sink, is_default }>`
  via GStreamer `DeviceMonitor` filtered to `Audio/Source` and `Audio/Sink`;
  emit add/remove events for hotplug.
- Replace `autoaudiosrc`/`autoaudiosink` with `pulsesrc`/`pulsesink` driven by a
  selected `device=` (falls back to default when unset).

### Unit 2 – Voice DSP, meter & activation (`engine::audio::capture`, `monitor`, `dsp`)

- **`engine::audio::dsp`** wraps the `webrtc-audio-processing` crate: a `Processor`
  configured with AEC, NS (level), AGC, VAD + high-pass, processing 10 ms frames
  of interleaved i16 at 48 kHz. Config setters apply **live** (no pipeline
  rebuild), so toggles flip instantly.
- Send branch bridges PCM through it:
  `pulsesrc device ! audioconvert ! audioresample (48k,S16,frames) ! appsink`
  → `dsp.process_capture_frame()` → `appsrc ! level ! mic_valve ! opusenc`.
  A second `appsink` tap off the playback (`pulsesink`) path feeds
  `dsp.process_render_frame()` so AEC has the far-end reference.
- `level` posts RMS/peak → drives the **sensitivity meter** and the
  voice-activity comparison; the crate's VAD also reports speech probability.
- **Activation** drives `mic_valve`: VAD-threshold (open above the chosen RMS /
  VAD), push-to-talk (valve follows the key), always-on (valve open). Mute
  overrides all.
- **Monitor pipeline** (`monitor.rs`): the same capture→DSP→`level`→`pulsesink`
  bridge for **Mic Test** + meter while *not* in a call (your voice looped to your
  speaker), reusing the same `Processor` settings.

### Unit 3 – Screenshare sources, audio & quality (`engine::screen::*`)

- **Video source** (`sources.rs`): enumerate the X root window's
  `_NET_CLIENT_LIST` for windows (id, title, small thumbnail) plus each
  monitor; capture with `ximagesrc` (whole screen) or `ximagesrc xid=<win>`.
- **Quality** (`capture.rs`): resolution (480/720/1080/1440/2160), fps
  (15/30/60), content-type Smoothness↔Clarity → capture caps + encoder
  bitrate/tuning; a `tee` feeds a local `gtk4paintablesink` for **preview**.
- **Audio** (`audio.rs`): list PipeWire output nodes (apps) with the venmic-style
  filters; build the Screen flow's audio track from
  `pipewiresrc target-object=<node>` (specific app) or
  `pulsesrc device=<default-sink>.monitor` (Entire System), always excluding our
  own process; encode **stereo 48 kHz Opus, no DSP**; add it to the Screen
  `webrtcbin` alongside HEVC. Viewers play the screenshare audio. Gated on
  PipeWire being present.

### Unit 4 – Live-apply & global push-to-talk (`engine::hotkey`, `session`)

- DSP toggle change = update the `Processor` config in place (no pipeline touch).
  Device/source change = pad-block the source, relink the new sub-branch, unblock
  (sub-second blip).
- Screenshare audio-source change while live = swap the audio sub-branch the same
  way.
- **Global PTT** (`hotkey.rs`): grab a chosen key on the X root window via
  `XGrabKey` (x11rb), report press/release to the activation gate; ordinary keys
  need no special permissions. In-app GTK key handler as a fallback.

### Unit 5 – Desktop settings & pickers

- **Settings window** (gear in the self-panel), **Voice page**: Microphone /
  Speaker dropdowns, Mic/Speaker volume, **Mic Test** + meter, Input-Sensitivity
  slider (with the live meter behind it), Noise-Suppression (off/standard/high),
  Echo Cancellation, AGC, Voice-Activity, Activation mode (Voice-activity /
  Push-to-talk / Always-on) + PTT keybind capture.
- **Screen Share picker** (replaces the bare Share toggle): source grid
  (screens + windows, thumbnails) with a large preview of the selection;
  Resolution / Frame-Rate / Content-Type rows; Audio-Source dropdown (None /
  Entire System / app list); **Go Live**.
- **`meter.rs`**: a reusable level-meter `gtk::DrawingArea` fed by `level`
  messages.
- Settings persist in the desktop config (extend `config.rs`, a TOML/JSON beside
  the token) with change-listeners; applied to the live `Session` immediately.

## Data flow

- **Open settings → change mic device**: UI writes `Settings.input_device` →
  `Session` rebuilds the voice capture branch live (pad-block swap) → next packets
  use the new device; the monitor pipeline (if mic-testing) swaps too.
- **Toggle noise suppression**: UI → `Session` sets the `Processor` NS flag live;
  it applies to the shared DSP for every active voice flow + the monitor.
- **Speak (VAD) / hold PTT**: `level`/hotkey → activation gate opens `mic_valve`
  → audio transmits; meter reflects input the whole time.
- **Start share**: picker returns {source, res, fps, content-type, audio-source}
  → `Session::start_share` builds the Screen flow (HEVC video + optional Opus
  audio) and offers to each viewer; the local preview shows the captured frames.
- **Switch screenshare audio source mid-share**: UI → swap the audio sub-branch
  on the Screen `webrtcbin` (no re-offer of the whole flow).

## Persistence

`config.rs` gains a `settings` section:

```
input_device, output_device, input_volume, output_volume,
noise_suppression (off|standard|high), echo_cancellation, agc, vad,
input_sensitivity, activation_mode (voice|ptt|always), ptt_key,
share_resolution, share_fps, share_content_type, share_audio_source
```

Local file only. No backend, protocol, or DB changes.

## Prerequisites (Linux dev box)

Already satisfied on the dev box (verified 2026-06-22):

- **GStreamer audio/screen elements** – `gstreamer1.0-pipewire`,
  `gstreamer1.0-pulseaudio` installed; `level`, `pulsesrc`, `pipewiresrc`,
  `ximagesrc` all present.
- **Voice DSP build deps** – the `webrtc-audio-processing` Rust crate builds a
  vendored libwebrtc copy via **autotools (libtool / automake / autoconf) +
  pkg-config** (all present); the system also has
  `libwebrtc-audio-processing 0.3.1`.

Note: the GStreamer `webrtcdsp`/`webrtcechoprobe` elements are **not provided** by
Ubuntu Noble (`gst-inspect-1.0 webrtcdsp` → not found), which is why DSP runs
in-process via the crate rather than in-pipeline. X11 window enumeration (`x11rb`)
and global PTT (`XGrabKey` via XCB) are pure-Rust cargo deps – no system package.

## Testing

- **TDD (engine logic)**: device-list parsing/labeling; settings
  serialization/round-trip; the activation-gate state machine (mute > ptt > vad >
  always; threshold compare); PipeWire node-filter rules; DSP frame
  bridging (10 ms i16 framing round-trip).
- **Run-and-observe (DSP audio, capture, UI)**: device switch mid-call; NS/AEC/AGC
  audibly change; meter tracks speech; VAD gates; global PTT works unfocused;
  screen vs window capture; app-audio vs system-audio in a share; resolution/fps
  change; live preview. **Screenshare verification uses `HEARTH_CAPTURE`
  synthetic source** (the M6 rule – never grab the real `:0` screen for testing).
- Recorded in `desktop/README.md`.

## Risks / prerequisites

- **DSP via the `webrtc-audio-processing` crate** adds a C++ build step (autotools:
  libtool/automake/autoconf, all present) and a PCM bridge
  (`appsink`→process→`appsrc`) that must hold strict 10 ms / 48 kHz framing;
  mis-framing degrades AEC. Isolated in `audio::dsp` with a round-trip unit test.
- **Live source hot-swap** is the trickiest engine bit – isolated behind a
  pad-block helper, run-and-observe tested.
- **Global PTT on X11** via `XGrabKey` can conflict if another app grabs the same
  key; keep the key configurable, fall back to in-app.
- **App audio** depends on the app exposing a PipeWire node; "only speakers" and
  own-pid exclusion avoid feedback loops.
- **Echo cancellation on one machine** with shared mic/speakers is imperfect
  (expected; real per-machine devices in normal use).
- Adding an audio track to the Screen flow renegotiates that flow's SDP; verify it
  doesn't disturb the M6 multi-sharer switcher.
