# Voice Transport Research — getting to < 50 ms

_Goal (see `docs/VISION.md`): Mumble/TeamSpeak-class voice, < 50 ms mouth-to-ear
on LAN, Rust-native, no WebRTC compromises. Today we measure **151 ms** on
localhost. This documents why, and the options to fix it._

## Headline finding: the 151 ms is WebRTC overhead

We use GStreamer `webrtcbin` for the voice flow. webrtcbin is built on `rtpbin`
(RTP session + jitterbuffer) **plus** ICE + DTLS-SRTP + congestion control +
bundling. The GStreamer community reports that **replacing `webrtcbin` with plain
`udpsink`/`udpsrc` (i.e. `rtpbin` directly) drops latency by ~150–200 ms** — the
overhead is WebRTC's, not RTP's. Our measured 151 ms (cross-correlation, 0.999)
lines up almost exactly: **the whole budget is webrtcbin.** Opus (~26 ms), WASAPI
(~10–20 ms each way), and a small jitter buffer are tens of ms; the rest is
WebRTC.

Conclusion: we don't need a new codec or a protocol rewrite to hit < 50 ms — we
need to **drop webrtcbin from the voice path.**

## Options surveyed

### A. Keep GStreamer, drop webrtcbin — raw RTP/Opus over UDP  ← recommended first
Replace the voice `webrtcbin` with `rtpbin` + `udpsink`/`udpsrc`, keeping our
existing Opus encode/decode and the audio capture/DSP pipeline untouched. Set
`rtpjitterbuffer latency=10–20` (vs webrtcbin's 200 default). Our signaling
WebSocket already brokers peers — it just exchanges `ip:port` + SSRC/payload
instead of an SDP offer/answer.
- **Latency:** ~20–40 ms (jitterbuffer + Opus + device). Removes the ~150 ms.
- **Effort:** Low — it's a transport swap on one flow; all capture/DSP/encode and
  most of the signaling stay. Mirrors the existing GStreamer design.
- **Trade-off:** We add our own NAT traversal later (STUN/hole-punching) and
  encryption (SRTP via `srtpenc`/`srtpdec`, or app-level). For a LAN/P2P friend
  mesh this is fine; coturn already exists for relay.
- This **is** the Mumble model: thin RTP/Opus over UDP + a small adaptive buffer.

### B. Native audio path — `cpal` + `opus` + adaptive jitter buffer + custom UDP  ← state-of-the-art target
Bypass GStreamer for audio entirely:
- **Capture/playback:** [`cpal`](https://lib.rs/multimedia/audio) — direct
  WASAPI/CoreAudio/ALSA/AAudio with small buffers (lowest device latency; also
  the path to mobile).
- **Codec:** [`audiopus`]/[`opus`] bindings (Opus with in-band FEC + DTX).
- **Jitter buffer:** [`jittr`](https://crates.io/crates/jittr) — a binary-heap
  adaptive jitter buffer for "zero latency" Opus/RTP/UDP streams (reorders +
  drops late packets with minimal delay), or a custom Speex-style adaptive buffer
  like Mumble's.
- **Transport:** `tokio` UDP; optional ChaCha20-Poly1305 per-session encryption
  (cheap, no DTLS handshake latency).
- **Latency:** < 50 ms achievable (true Mumble/TeamSpeak class), full control,
  best for mobile.
- **Effort:** Higher — we own capture, PLC, jitter, transport. But it's the only
  path that is genuinely "state of the art" and not bottlenecked by a framework.

### C. Mumble protocol crate — [`mumble-protocol`](https://crates.io/crates/mumble-protocol)
Implements the actual Mumble wire format (TCP control + UDP voice, Opus,
sequence numbers, OCB-AES). Useful as a **reference** for packet design, or if we
ever wanted Mumble-server interop. We control both ends, so we don't need wire
compat — but it's the proven blueprint for B's packet/jitter design.

### D. `str0m` — sans-IO WebRTC in Rust
[`str0m`](https://github.com/algesten/str0m) is a modern, lock-free, sans-IO
WebRTC stack — much lighter and more idiomatic than `webrtc-rs`. **But it's still
WebRTC** (DTLS-SRTP, jitter, congestion control), so it keeps the very overhead
we're trying to remove. Worth knowing if we ever want browser interop; not the
low-latency direction.

### E. QUIC datagrams — [`quinn`](https://github.com/quinn-rs/quinn)
Unreliable QUIC datagrams can carry media, and the [`voices`](https://github.com/sebpuetz/voices)
project pairs QUIC + Opus. QUIC shines for the **reliable control channel**
(presence/signaling/chat) and gives encryption + congestion control, but its
handshake/streams add latency vs raw UDP for the voice datagrams themselves.
Candidate for the control plane, not the lowest-latency voice path.

## Latency budget comparison (LAN, rough)

| Path | Voice latency | Effort | Notes |
|---|---|---|---|
| webrtcbin (today) | **~151 ms** | — | measured |
| A: GStreamer RTP/UDP | ~20–40 ms | low | reuse engine; swap transport |
| B: cpal + opus + jittr | **< 50 ms** | high | state-of-the-art, mobile-ready |
| D: str0m | ~80–150 ms | med | still WebRTC overhead |
| E: QUIC datagrams | ~40–80 ms | med | better as control plane |

## The real floor is the OS audio engine, not us (measured)

Measuring with OBS (Mic track vs "Entire System" track, cross-correlated):
- **"Listen to this device"** (mic → speakers, near-zero app logic): **~37.5 ms**.
- That matches the public benchmark for a *normal Windows app's* click→sound
  (~36 ms) — and even CS2 measures ~96 ms. It's the **Windows shared-mode audio
  engine**, common to every app, not our pipeline.
- ASIO/exclusive gets ~3 ms but **locks the device** — unacceptable for a voice
  app that must coexist with the game's audio, the browser, system sounds. We do
  not use exclusive mode.

After moving voice off webrtcbin + Opus low-delay + play-on-arrival, our measured
one-way latency went **151 → 124 → 92 ms** (stable, no drift over 3 min). So our
own pipeline adds **~54 ms** on top of the ~37.5 ms OS floor: jitter buffer
(20 ms, ~the stability floor), the device capture/render periods, Opus (~13 ms),
and GStreamer `wasapi2` element overhead (~11 ms). The GStreamer path is near its
floor here.

### Breaking the OS floor — per platform (no exclusive lock)
"Shared mode" is not one number. Each OS has a low-latency shared path that
coexists with other apps:

| | Low-latency shared (what we want) | Exclusive (rejected) |
|---|---|---|
| **Windows** | WASAPI **`IAudioClient3`** — ~3–10 ms engine periods, no lock | WASAPI exclusive / ASIO (~3 ms, locks device) |
| **Linux** | **PipeWire** small quantum (~5 ms) — its design goal | JACK (~3 ms) |

`IAudioClient3` is Windows-only; Linux is a different stack entirely (PipeWire/
ALSA), and PipeWire is *better* suited — low-latency shared audio is what it's
built for. `cpal` abstracts device I/O but uses the default `IAudioClient`/ALSA
path and does **not** expose `IAudioClient3`/PipeWire low-latency modes, so the
Phase-2 audio layer likely needs a thin per-platform shim (or a `cpal` fork)
targeting `IAudioClient3` on Windows and a small PipeWire quantum on Linux. This
is where the ~37.5 ms floor actually drops.

## Recommendation: two phases

1. **Phase 1 — swap voice transport to raw RTP/Opus over UDP in GStreamer
   (Option A).** Biggest latency win for the least work; keeps the proven capture
   /DSP/encode. Gets us from 151 ms to ~30 ms now. Add `srtpenc`/`srtpdec` for
   encryption and STUN hole-punching for non-LAN.
2. **Phase 2 — native `cpal` + `opus` + adaptive jitter buffer + custom UDP
   (Option B), borrowing Mumble's packet/jitter design (Option C).** The true
   state-of-the-art endpoint: full control, lowest latency, the foundation for
   mobile. Migrate once Phase 1 proves the transport/signaling shape.

## Screenshare transport — Moonlight-class (~20–30 ms)

Same story as voice: the screen flow also runs over `webrtcbin` today, so it
carries the same ~150 ms overhead — fatal to the **Moonlight/Sunshine** target
(~20 ms gaming). Moonlight's recipe: GPU **HW encode** (NVENC/AMF/QSV) → thin UDP
transport with **Reed-Solomon FEC** (recover loss without retransmit/jitter
stalls) → GPU **HW decode** → tight frame pacing. We already have the host side
on Windows (`d3d11screencapturesrc` + `amfh265enc`); the gaps are:

1. **Transport:** drop `webrtcbin` for the screen flow → `rtpbin`/`udpsink`/
   `udpsrc` (HEVC RTP) with FEC (`rtpulpfecenc`/`rtpulpfecdec`, or app-level
   Reed-Solomon as Moonlight uses). Keep GStreamer for the media elements.
2. **Receiver HW decode:** replace software `avdec_h265` with `d3d11h265dec` /
   `nvh265dec` (lower latency + frees the CPU). Pair with the GTK4 paintable.
3. **Frame pacing + minimal buffering** on the receive side.

GStreamer stays — it's best-in-class for the GPU capture/encode/decode elements;
only the **transport** (webrtcbin) is swapped, exactly like voice. Screenshare
**audio is captured per-source and isolated** from the video pipeline so it
matches the chosen video source (OBS model) and never blacks the video.

References: [Sunshine](https://github.com/LizardByte/Sunshine) ·
[Moonlight docs](https://github.com/moonlight-stream/moonlight-docs/wiki/Frequently-Asked-Questions).

## Sources
- [GStreamer rtpbin](https://gstreamer.freedesktop.org/documentation/rtpmanager/rtpbin.html) ·
  [rtpjitterbuffer](https://gstreamer.freedesktop.org/documentation/rtpmanager/rtpjitterbuffer.html) ·
  [WebRTC vs UDP/RTP latency discussion](https://discourse.gstreamer.org/t/webrtc-vs-udp-rtp-streaming-audio-samples-to-a-remote-compute/5476)
- [rust-mumble-protocol](https://github.com/Johni0702/rust-mumble-protocol) ·
  [mumble-protocol crate](https://crates.io/crates/mumble-protocol)
- [jittr jitter buffer](https://crates.io/crates/jittr) ·
  [Rust audio crates (lib.rs)](https://lib.rs/multimedia/audio) ·
  [voices (QUIC+Opus)](https://github.com/sebpuetz/voices)
- [str0m sans-IO WebRTC](https://github.com/algesten/str0m) ·
  [quinn QUIC](https://github.com/quinn-rs/quinn)
