# Hearth — Product Vision & Non-Negotiables

_The north star for this project. Read this before making architecture decisions.
If a change trades away one of these goals for convenience, it's the wrong change._

## What we are building

A **state-of-the-art, low-latency, native** voice + screenshare app for a small
group of close friends. **Native desktop first (Windows + Linux), phones later.**
Not a web app, not Electron, not a federated platform.

The benchmark is **OBS** (for capture quality/flexibility) and **Mumble /
TeamSpeak** (for voice latency and reliability) — _not_ Discord. Discord
compromises latency and quality for scale, browser support, and moderation we do
not need. We have none of those constraints, so we do not accept those
compromises.

## Non-negotiables

1. **Lowest possible audio latency.** Target **< 50 ms** mouth-to-ear on
   LAN/localhost, and as low as the network allows otherwise. 150 ms on the same
   machine is unacceptable. Mumble/TeamSpeak achieve < 50 ms; so must we.
1b. **Moonlight-class screenshare latency.** Target **~20–30 ms glass-to-glass**
   for gaming, the way Moonlight/Sunshine do it: GPU hardware **encode AND
   decode** (NVENC/AMF/QSV ↔ d3d11/nvdec), a thin UDP transport with **FEC** (no
   fat jitter buffer, no retransmit stalls), and tight frame pacing. This is the
   original reason for the project — it is not negotiable and was never dropped.
2. **Reliability like Mumble/TeamSpeak.** Rock-solid voice that doesn't drift,
   accumulate delay, or drop the call. Adaptive jitter handling + packet-loss
   concealment, not a fixed fat buffer.
3. **OBS-style per-source A/V selection.** The user picks a *source* and gets
   both its video and its matching audio, with no compromise:
   - Whole screen → whole-screen video (+ optional whole-system audio)
   - A window (e.g. Firefox) → that window's video **and** that app's audio
   - A game (e.g. CS2) → that game's video **and** that game's audio
   Per-application audio is a **requirement**, not a nice-to-have. OBS does this
   (Game Capture + Application Audio Capture); so can we.
4. **Three independent media flows.** Voice, screenshare (video **+** its audio),
   and webcam are separate streams that drop/congest independently — never one
   bundled wire where one stalls the rest.
5. **No self-echo.** A user must never hear themselves: shared audio excludes the
   call's own output; capture excludes our own playback.
6. **GPU-accelerated, native capture/encode AND decode.** Per-OS best path
   (Windows: WGC/DXGI capture + AMF/NVENC/QSV encode + d3d11/nvdec **decode**;
   Linux: PipeWire/X11 + VAAPI both ways; macOS later). The host side (d3d11
   capture + AMF HEVC encode) already works on Windows; the **receiver must HW-
   decode** too (currently software `avdec_h265`, a latency/CPU cost to remove).
   CPU paths are last-resort fallback only.

## Explicit direction (decided)

- **Audio transport is being moved off WebRTC.** WebRTC's jitter buffer + DTLS +
  congestion control add latency and complexity we do not want for a trusted
  P2P friend mesh. We will use a **thin low-latency UDP voice transport**
  (Mumble-protocol-class) with our own small adaptive jitter buffer + Opus FEC.
  Encryption stays (it's cheap), but not at the cost of latency. See
  `docs/research/voice-transport.md`.
- **Screenshare also moves off WebRTC.** `webrtcbin` adds ~150 ms (measured for
  voice; the same overhead applies to the screen flow) — fatal to the
  Moonlight-class target. The screen flow uses the same thin UDP transport with
  FEC for loss (Moonlight/Sunshine model), keeping GStreamer for the GPU
  capture/encode/decode elements. The host capture+encode (d3d11 + AMF) already
  exists; what's left is the transport swap + receiver HW decode + frame pacing.
- **Screenshare A/V is captured per-source and isolated** so an audio failure can
  never blank the video, and each video source carries its matching audio.

> History note: WebRTC (`webrtcbin`) was the original engine's transport choice
> (pre-Windows-port), bundling all three flows. It is the shared latency
> bottleneck for **both** voice and screenshare and is being removed for both —
> the Moonlight/low-latency goal was never abandoned, just not yet reached while
> the Windows port and the rest of the stack came up.

## How we measure success

- Mouth-to-ear voice latency (cross-correlation of mic vs. far-end capture):
  **< 50 ms LAN**, reported per release.
- Screenshare glass-to-glass latency and 1080p/60 legibility under motion.
- Per-app audio works for browser/game sources, reliably, without blacking video.
- Zero self-echo in a live call while sharing system audio.
