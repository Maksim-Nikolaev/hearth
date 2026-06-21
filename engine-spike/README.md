# engine-spike (M2 media risk spike)

Throwaway crate that validates whether GStreamer `webrtcbin` can deliver
hardware-encoded, low-latency, high-fidelity screenshare P2P on the target
(AMD/X11) hardware. Not product code.

## Modes

```bash
cargo run -- probe     # list available HW encoders, pick the best
cargo run -- local     # ximagesrc -> HW HEVC -> decode -> window (capture+encode proof)
cargo run -- answer    # webrtcbin answerer (run first)
cargo run -- offer     # webrtcbin offerer (run second); shares this screen to the answerer
```

The `offer`/`answer` peers exchange SDP + ICE via files in `/tmp`
(`hearth_offer.sdp`, `hearth_answer.sdp`, `hearth_ice_offer.txt`,
`hearth_ice_answer.txt`) so the spike needs no signaling server. Delete those
files between runs.

## System prerequisites (Ubuntu/Mint)

Runtime: `gstreamer1.0-plugins-base/good/bad`, `gstreamer1.0-vaapi` or the `va`
plugins, `gstreamer1.0-nice` (libnice, required for `webrtcbin` ICE).
Build: `libgstreamer1.0-dev`, `libgstreamer-plugins-base1.0-dev`,
`libgstreamer-plugins-bad1.0-dev`, `pkg-config`.

## Measurements log

### Encoder probe (2026-06-21, AMD box)

```
[ ] amfh265enc     AMD AMF HEVC
[x] vah265enc      VA-API HEVC (modern)
[x] vaapih265enc   VA-API HEVC (legacy)
[ ] nvh265enc      NVIDIA NVENC HEVC
[ ] qsvh265enc     Intel QuickSync HEVC
[ ] vtenc_h265     Apple VideoToolbox HEVC
[x] x265enc        software HEVC (fallback)

selected encoder: Some("vah265enc")
```

VA-API exposes hardware **HEVC** (Main + Main10) and **AV1** encode entrypoints
(`vainfo`); a headless `ximagesrc -> vah265enc -> fakesink` 30-frame encode
completed in ~1.2 s. Hardware encode path confirmed.

### Local pipeline (B) (2026-06-21, AMD box)

Full local chain proven: `ximagesrc -> videoconvert -> vah265enc -> h265parse ->
avdec_h265 -> videoconvert -> fakesink` ran 60 frames in ~2.6 s with a clean EOS
(HW HEVC encode **and** decode both working). `autovideosink` instantiates and
accepts a frame (EOS in ~40 ms, no error), so the on-screen display path is good.
Capture + hardware encode + decode + display all confirmed on one machine.

### Two-peer webrtcbin (C)

_TBD on run._

### Go/No-Go decision

_TBD after cross-machine measurement._
