//! Native voice transport (Phase 2) — replaces the GStreamer `voice_udp` path
//! when `HEARTH_NATIVE_AUDIO` is set.
//!
//! Shape (no GStreamer, no `webrtcbin`):
//! - **Send (shared):** one [`NativeCapture`] → gate (mute/PTT/VAD, the same
//!   [`Gate`]) → Opus `encode_float` → the encoded 10 ms frame is sent to every
//!   peer's endpoint.
//! - **Receive (per peer):** a UDP socket per peer → Opus `decode_float` → a
//!   mixer lane in the single [`NativePlayback`].
//!
//! Endpoints are exchanged exactly like `voice_udp`: the `Offer`/`Answer` carry
//! `"ip:port"`. The session drives [`add_peer`](NativeVoice::add_peer) /
//! [`set_remote`](NativeVoice::set_remote) / [`remove_peer`](NativeVoice::remove_peer).

use super::drift::{DriftServo, Varispeed};
use super::gate::Gate;
use super::jitter::{JitterBuffer, JitterCounters, JitterOut};
use super::native::{NativeCapture, NativePlayback};
use super::voice_packet::{self, Timing};
use crate::session::SessionEvent;
use anyhow::{anyhow, Result};
use super::dsp::AecMethod;
use super::speex_aec::SpeexAec;
#[cfg(not(target_os = "windows"))]
use super::webrtc_aec::WebrtcAec;
use audiopus::{coder::Decoder, coder::Encoder, Application, Channels, SampleRate};
use earshot::{VoiceActivityDetector, VoiceActivityProfile};
use nnnoiseless::DenoiseState;

/// AEC frame (10 ms) and adaptive-filter tail (~100 ms of room echo) at 48 kHz.
const AEC_FRAME: usize = 480;
const AEC_FILTER_LEN: i32 = 4800;
use std::collections::HashMap;
use std::net::{SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::mpsc::UnboundedSender;
use uuid::Uuid;

/// Opus frame: 5 ms @ 48 kHz mono. Smaller frame = less packetization delay and
/// a shallower playback lane (one frame) than the 10 ms default.
const FRAME: usize = 240;

/// Frame duration in microseconds (`FRAME` / 48 kHz) — the receive release tick.
const FRAME_US: u64 = FRAME as u64 * 1_000_000 / 48_000;

/// Default receive jitter-buffer depth (≈20 ms at the 5 ms framing). Overridden
/// live by the Settings "Jitter buffer (ms)" slider via [`NativeVoice::set_jitter_ms`].
const DEFAULT_JITTER_MS: u32 = 20;

/// Device-lane cushion the refill loop keeps filled (~10 ms). The jitter buffer
/// holds the network cushion; this only has to outlast the gap between refills so
/// the render thread never drains the lane dry. The release thread raises the OS
/// timer resolution to 1 ms (see `TimerResolutionGuard`) so `LANE_POLL_MS` sleeps
/// are honored on Windows too, keeping two frames enough without the silence gaps
/// the default ~15.6 ms Windows tick caused.
const TARGET_LANE_FRAMES: usize = 2;

/// Receive servo poll period — short enough that the lane can't fall below the
/// cushion between checks while the device drains it at the hardware rate.
const LANE_POLL_MS: u64 = 3;

/// RNNoise frame: 10 ms @ 48 kHz. NS buffers this much (so enabling it adds
/// ~10 ms of latency over the 5 ms Opus framing).
const NS_FRAME: usize = DenoiseState::FRAME_SIZE; // 480

/// A send destination, shared between the capture/encode thread and the session.
/// The remote is filled in once the peer's endpoint arrives over signaling.
struct SendTarget {
    sock: Arc<UdpSocket>,
    remote: Mutex<Option<SocketAddr>>,
    /// Latest `send_ts` seen from this peer, echoed in our outbound timed packets
    /// so the peer can compute RTT. 0 = none seen yet (also the stats-off value).
    peer_send_ts: AtomicU32,
}

struct Peer {
    source_id: u64,
    target: Arc<SendTarget>,
    stop: Arc<AtomicBool>,
    /// The peer's receive threads: the socket reader and the paced release loop.
    handles: Vec<std::thread::JoinHandle<()>>,
    /// Receive jitter buffer (owns the packet counters) and the smoothed RTT in
    /// ms (0 = unknown), both read by [`NativeVoice::stats_snapshot`].
    jitter: Arc<Mutex<JitterBuffer>>,
    rtt_ms: Arc<AtomicU32>,
}

/// Per-peer voice diagnostics snapshot (one row of the stats readout).
#[derive(Clone, Copy)]
pub struct PeerStatsSnapshot {
    pub peer: Uuid,
    /// Smoothed round-trip time, `None` until a timed echo returns.
    pub rtt_ms: Option<u32>,
    /// Current playout buffering: jitter-buffer depth + device-lane cushion (ms).
    pub jitter_ms: u32,
    pub lane_ms: u32,
    pub counters: JitterCounters,
}

impl PeerStatsSnapshot {
    /// Network loss fraction in percent: concealed / (accepted + concealed).
    pub fn loss_pct(&self) -> f32 {
        let denom = self.counters.accepted + self.counters.concealed;
        if denom == 0 {
            0.0
        } else {
            self.counters.concealed as f32 / denom as f32 * 100.0
        }
    }
}

/// The active echo canceller, chosen by [`AecMethod`]. Rebuilt when the method
/// changes (rare); `set_strength` adjusts it live. On Windows the `Webrtc`
/// variant is unavailable, so `build` falls back to speex there.
enum AecInner {
    Speex(SpeexAec),
    #[cfg(not(target_os = "windows"))]
    Webrtc(WebrtcAec),
}

impl AecInner {
    /// Speex at a fixed-aggressive internal config; the user-facing strength is
    /// the wet/dry blend in [`AecImpl::cancel_frame`], not speex's residual knob.
    fn speex() -> Self {
        AecInner::Speex(SpeexAec::new(AEC_FRAME, AEC_FILTER_LEN, 48000, 100))
    }
}

/// `strength` (0–100) as a 0.0–1.0 wet/dry fraction.
fn wet_from(strength: u8) -> f32 {
    strength.min(100) as f32 / 100.0
}

struct AecImpl {
    inner: AecInner,
    /// Speex wet/dry mix: 0 = raw mic (no cancellation, = off), 1 = fully
    /// cancelled. Unused by WebRTC, whose `strength` drives its suppression level
    /// while it always runs at full cancellation.
    wet: f32,
}

impl AecImpl {
    fn build(method: AecMethod, strength: u8) -> Self {
        let inner = match method {
            AecMethod::Speex => AecInner::speex(),
            #[cfg(not(target_os = "windows"))]
            AecMethod::Webrtc => match WebrtcAec::new(strength) {
                Ok(w) => AecInner::Webrtc(w),
                Err(e) => {
                    eprintln!("[native-voice] WebRTC AEC init failed ({e}); using speex");
                    AecInner::speex()
                }
            },
            #[cfg(target_os = "windows")]
            AecMethod::Webrtc => AecInner::speex(),
        };

        AecImpl { inner, wet: wet_from(strength) }
    }

    fn set_strength(&mut self, strength: u8) {
        self.wet = wet_from(strength);
        // WebRTC ignores the blend; its strength is a suppression level.
        #[cfg(not(target_os = "windows"))]
        if let AecInner::Webrtc(a) = &mut self.inner {
            a.set_strength(strength);
        }
    }

    /// Cancel echo on one [`AEC_FRAME`]-sample mic frame, appending the cleaned
    /// f32 samples to `out`. `far` is the matching rendered playback frame.
    fn cancel_frame(&mut self, mic: &[f32], far: &mut [f32; AEC_FRAME], out: &mut Vec<f32>) {
        let wet = self.wet;
        match &mut self.inner {
            AecInner::Speex(a) => {
                let to_i16 = |s: &f32| (s.clamp(-1.0, 1.0) * 32767.0) as i16;
                let far_i: Vec<i16> = far.iter().map(to_i16).collect();
                let mic_i: Vec<i16> = mic.iter().map(to_i16).collect();
                let mut out_i = [0i16; AEC_FRAME];
                a.cancel(&mic_i, &far_i, &mut out_i);
                // Blend cancelled vs raw mic so strength spans off (0) → full (1).
                for (i, c) in out_i.iter().enumerate() {
                    let cancelled = *c as f32 / 32768.0;
                    out.push(wet * cancelled + (1.0 - wet) * mic[i]);
                }
            }
            #[cfg(not(target_os = "windows"))]
            AecInner::Webrtc(a) => {
                let mut mic_buf: Vec<f32> = mic.to_vec();
                a.cancel(&mut mic_buf, far);
                out.extend_from_slice(&mic_buf);
            }
        }
    }
}

/// `AecMethod` as an atomic-friendly u8 and back.
fn method_to_u8(m: AecMethod) -> u8 {
    match m {
        AecMethod::Speex => 0,
        AecMethod::Webrtc => 1,
    }
}
fn method_from_u8(v: u8) -> AecMethod {
    match v {
        1 => AecMethod::Webrtc,
        _ => AecMethod::Speex,
    }
}

pub struct NativeVoice {
    _capture: NativeCapture,
    playback: Arc<NativePlayback>,
    /// Send destinations the capture/encode thread iterates each frame.
    targets: Arc<Mutex<Vec<Arc<SendTarget>>>>,
    peers: HashMap<Uuid, Peer>,
    deaf: Arc<AtomicBool>,
    /// Temporary output silence (e.g. while Settings is open), independent of
    /// the user's deafen state.
    suspended: Arc<AtomicBool>,
    /// RNNoise wet/dry mix in permille (0 = off, 1000 = full). RNNoise itself is
    /// binary, so the Off/Low/Moderate/High level blends denoised vs original to
    /// give "how much to reduce".
    ns_wet: Arc<AtomicU32>,
    /// Mic-test self-monitor (loop your own audio back to your speakers).
    self_monitor: Arc<AtomicBool>,
    /// earshot VAD on/off, and AGC on/off (toggled from Settings).
    vad: Arc<AtomicBool>,
    agc: Arc<AtomicBool>,
    /// speexdsp AEC on/off.
    ec: Arc<AtomicBool>,
    /// Residual-echo suppression strength (0–100), applied live.
    ec_strength: Arc<AtomicU32>,
    /// User mic volume (pre-amp, f32 bits, 0.0–1.0), applied live.
    input_volume: Arc<AtomicU32>,
    /// Active echo-canceller method (speex/webrtc) as a u8, applied live.
    aec_method: Arc<AtomicU8>,
    /// Receive jitter-buffer depth in milliseconds, shared with every peer's
    /// release loop so the Settings slider retunes the buffer mid-call.
    jitter_ms: Arc<AtomicU32>,
    /// Monotonic origin for RTT timestamps (ms since this transport started).
    epoch: Instant,
    next_source: u64,
}

impl NativeVoice {
    /// Start the shared capture + playback. Mic frames are gated and Opus-encoded
    /// on the capture thread and sent to every registered peer.
    pub fn new(
        gate: Arc<Mutex<Gate>>,
        evt_tx: UnboundedSender<SessionEvent>,
        input_device: Option<String>,
        output_device: Option<String>,
        ns_wet: u32,
        vad: bool,
        agc: bool,
        ec: bool,
        ec_strength: u8,
        aec_method: AecMethod,
        voice_status: crate::session::VoiceStatus,
    ) -> Result<Self> {
        #[cfg(windows)]
        eprintln!("[native-voice] active — WASAPI IAudioClient3 capture/playback + Opus + UDP");
        #[cfg(target_os = "linux")]
        eprintln!("[native-voice] active — PipeWire capture/playback + Opus + UDP");
        let playback = Arc::new(NativePlayback::start(output_device)?);
        let targets: Arc<Mutex<Vec<Arc<SendTarget>>>> = Arc::new(Mutex::new(Vec::new()));
        let deaf = Arc::new(AtomicBool::new(false));
        let ns_wet = Arc::new(AtomicU32::new(ns_wet));
        // User mic volume (pre-amp, f32 bits, 0.0–1.0); 1.0 = unity passthrough.
        let input_volume = Arc::new(AtomicU32::new(1.0f32.to_bits()));
        let input_vol_cb = input_volume.clone();

        let encoder = Encoder::new(SampleRate::Hz48000, Channels::Mono, Application::LowDelay)?;
        let mut acc: Vec<f32> = Vec::with_capacity(FRAME * 2);
        // Opus payload is encoded once per frame, then `voice_packet::encode`
        // frames it per peer (a different echo timestamp each). See `voice_packet`.
        let mut payload = vec![0u8; 4000];
        let mut packet = vec![0u8; 4096];
        let mut seq: u16 = 0;
        let targets_cb = targets.clone();

        // RTT epoch (ms since start) and whether we stamp timing (default on).
        let epoch = Instant::now();
        let stats_on = voice_packet::stats_enabled();

        // NS pre-stage state (RNNoise works in 480-sample i16-range frames).
        let ns_wet_cb = ns_wet.clone();
        let mut denoiser = DenoiseState::new();
        let mut ns_in: Vec<f32> = Vec::with_capacity(NS_FRAME * 2);
        let mut ns_out: Vec<f32> = Vec::new();
        // Speaking indicator: on when the gate transmits, with ~200 ms hangover so
        // it doesn't flicker between words.
        let mut speaking = false;
        let mut silent_frames = 0usize;
        const SPEAKING_HANGOVER: usize = 40; // 40 × 5 ms = 200 ms

        // Self-monitor (mic test): loop our own captured audio back to playback so
        // you can hear yourself with the real processing chain, even in a call.
        const SELF_MONITOR_LANE: u64 = u64::MAX;
        let self_monitor = Arc::new(AtomicBool::new(false));
        let self_monitor_cb = self_monitor.clone();
        let mon_playback = playback.clone();

        // VAD (earshot): real speech detection feeding the gate's Voice-activity
        // mode + the speaking indicator. Processes 480-sample (10 ms) 48 kHz i16
        // frames; QUALITY profile preserves quiet speech.
        let vad_enabled = Arc::new(AtomicBool::new(vad));
        let vad_cb = vad_enabled.clone();
        let mut detector = VoiceActivityDetector::new(VoiceActivityProfile::QUALITY);
        let mut vad_acc: Vec<i16> = Vec::with_capacity(960);
        let mut last_vad = false;

        // AGC: simple envelope-follower bringing speech toward a target level.
        let agc_enabled = Arc::new(AtomicBool::new(agc));
        let agc_cb = agc_enabled.clone();
        let mut agc_gain = 1.0f32;

        // Gate de-click: smoothly ramp the applied gain in/out (~10 ms) so opening
        // and closing the gate doesn't produce a click. Separate envelopes for the
        // send path and the self-monitor (they gate on different decisions).
        let mut send_gain = 0.0f32;
        let mut mon_gain = 0.0f32;

        // AEC: cancel the speaker mix (far-end, tapped from playback) out of the
        // mic. speexdsp processes fixed 480-sample i16 frames; buffer to that.
        let ec_enabled = Arc::new(AtomicBool::new(ec));
        let ec_cb = ec_enabled.clone();
        let far_ring = playback.far_end();
        let mut aec = AecImpl::build(aec_method, ec_strength);
        let mut aec_in: Vec<f32> = Vec::with_capacity(AEC_FRAME * 2);
        // Echo-canceller method + residual strength, adjusted live from Settings.
        // The closure rebuilds `aec` on a method change and re-applies strength on
        // a strength change.
        let aec_method_state = Arc::new(AtomicU8::new(method_to_u8(aec_method)));
        let aec_method_cb = aec_method_state.clone();
        let ec_strength_state = Arc::new(AtomicU32::new(ec_strength as u32));
        let ec_strength_cb = ec_strength_state.clone();
        let mut last_strength = ec_strength;
        let mut last_method = aec_method;

        eprintln!(
            "[native-voice] filters: ns_wet={} vad={vad} agc={agc} ec={ec} ec_strength={ec_strength} aec={aec_method:?}",
            ns_wet.load(Ordering::Relaxed)
        );

        let capture = NativeCapture::start(input_device, move |mono| {
            // Mic pre-amp (user input volume) before AEC/DSP, so the gate/meter and
            // everything downstream see the adjusted level.
            let in_vol = f32::from_bits(input_vol_cb.load(Ordering::Relaxed));
            let mut in_scaled: Vec<f32> = Vec::new();
            let mono: &[f32] = if (in_vol - 1.0).abs() > f32::EPSILON {
                in_scaled.extend_from_slice(mono);
                crate::audio::capture::apply_gain(&mut in_scaled, in_vol);
                &in_scaled
            } else {
                mono
            };

            // AEC first (on the raw mic), so NS/AGC see echo-free audio. When off,
            // `src` is the mic untouched.
            let ec_on = ec_cb.load(Ordering::Relaxed);
            if ec_on {
                // Apply live method/strength changes to the active canceller.
                let cur = ec_strength_cb.load(Ordering::Relaxed) as u8;
                let method = method_from_u8(aec_method_cb.load(Ordering::Relaxed));
                if method != last_method {
                    aec = AecImpl::build(method, cur);
                    last_method = method;
                    last_strength = cur;
                } else if cur != last_strength {
                    aec.set_strength(cur);
                    last_strength = cur;
                }
            }
            let cleaned: Vec<f32> = if ec_on {
                aec_in.extend_from_slice(mono);
                let mut out = Vec::with_capacity(aec_in.len());
                while aec_in.len() >= AEC_FRAME {
                    let mic_f: Vec<f32> = aec_in.drain(..AEC_FRAME).collect();
                    // Far-end (rendered playback) frame as f32; speex converts to
                    // i16 internally, WebRTC consumes f32 directly.
                    let mut far_f = [0.0f32; AEC_FRAME];
                    {
                        let mut fr = far_ring.lock().unwrap();
                        for d in far_f.iter_mut() {
                            *d = fr.pop_front().unwrap_or(0.0);
                        }
                    }
                    aec.cancel_frame(&mic_f, &mut far_f, &mut out);
                }
                out
            } else {
                aec_in.clear();
                Vec::new()
            };
            let mono: &[f32] = if ec_on { &cleaned } else { mono };

            // Voice activity detection on the raw mic (10 ms / 480-sample frames).
            if vad_cb.load(Ordering::Relaxed) {
                vad_acc.extend(mono.iter().map(|s| (s.clamp(-1.0, 1.0) * 32767.0) as i16));
                while vad_acc.len() >= 480 {
                    let chunk: Vec<i16> = vad_acc.drain(..480).collect();
                    last_vad = detector.predict_48khz(&chunk).unwrap_or(false);
                }
            } else {
                last_vad = false;
                vad_acc.clear();
            }

            // Optional noise suppression before the Opus path. RNNoise expects
            // f32 in i16 amplitude range and fixed 480-sample frames; buffer to
            // that, denoise, and blend denoised vs original by the wet level.
            let wet = ns_wet_cb.load(Ordering::Relaxed);
            let feed: &[f32] = if wet > 0 {
                let wetf = wet as f32 / 1000.0;
                ns_in.extend_from_slice(mono);
                ns_out.clear();
                while ns_in.len() >= NS_FRAME {
                    let raw: Vec<f32> = ns_in.drain(..NS_FRAME).collect();
                    let mut inp = [0.0f32; NS_FRAME];
                    let mut outp = [0.0f32; NS_FRAME];
                    for (d, s) in inp.iter_mut().zip(raw.iter()) {
                        *d = s * 32768.0;
                    }
                    denoiser.process_frame(&mut outp, &inp);
                    for (i, o) in outp.iter().enumerate() {
                        let den = (o / 32768.0).clamp(-1.0, 1.0);
                        ns_out.push((wetf * den + (1.0 - wetf) * raw[i]).clamp(-1.0, 1.0));
                    }
                }
                &ns_out
            } else {
                mono
            };

            acc.extend_from_slice(feed);
            while acc.len() >= FRAME {
                let mut frame: Vec<f32> = acc.drain(..FRAME).collect();

                // AGC: adapt a smoothed gain toward a target level (only on real
                // signal, so it doesn't pump up the noise floor), then apply.
                if agc_cb.load(Ordering::Relaxed) {
                    let r = (frame.iter().map(|s| s * s).sum::<f32>() / FRAME as f32).sqrt();
                    if r > 0.005 {
                        let desired = (0.1 / r).clamp(0.1, 4.0); // target ~ -20 dBFS
                        agc_gain += 0.04 * (desired - agc_gain);
                    } else {
                        // Silence: relax toward unity so we don't crank up the noise
                        // floor (was holding the last, possibly large, gain).
                        agc_gain += 0.05 * (1.0 - agc_gain);
                    }
                    for s in frame.iter_mut() {
                        *s = crate::audio::native::soft_clip(*s * agc_gain);
                    }
                }

                let rms_db = rms_dbfs(&frame);
                let (open, mon_open) = {
                    let mut g = gate.lock().unwrap();
                    g.update_level(rms_db, last_vad);
                    (g.open(), g.monitor_open())
                };
                let _ = evt_tx.send(SessionEvent::InputLevel(rms_db));

                // Mic test: hear yourself, ramped in/out so crossing the threshold
                // doesn't click. (ignores mute / the Settings-open suspend.)
                if self_monitor_cb.load(Ordering::Relaxed) {
                    let mut mon = frame.clone();
                    ramp_gain(&mut mon, &mut mon_gain, if mon_open { 1.0 } else { FLOOR_GAIN });
                    mon_playback.push(SELF_MONITOR_LANE, &mon);
                } else {
                    mon_gain = 0.0;
                }

                // Broadcast speaking transitions (with hangover). "Speaking" =
                // actually transmitting voice: the gate is open AND there's real
                // signal (so Always-on / held-PTT don't show talking while silent).
                if open && rms_db > -50.0 {
                    silent_frames = 0;
                    if !speaking {
                        speaking = true;
                        voice_status.set_speaking(true);
                        let _ = evt_tx.send(SessionEvent::SelfSpeaking(true));
                    }
                } else {
                    silent_frames += 1;
                    if speaking && silent_frames > SPEAKING_HANGOVER {
                        speaking = false;
                        voice_status.set_speaking(false);
                        let _ = evt_tx.send(SessionEvent::SelfSpeaking(false));
                    }
                }

                // Ramp the send gain in/out for a click-free open/close, then encode
                // the faded frame — resting at the gate floor once fully closed.
                ramp_gain(&mut frame, &mut send_gain, if open { 1.0 } else { FLOOR_GAIN });
                let pn = match encoder.encode_float(&frame, &mut payload) {
                    Ok(n) => n,
                    Err(_) => continue,
                };

                let send_ts = epoch.elapsed().as_millis() as u32;
                for t in targets_cb.lock().unwrap().iter() {
                    if let Some(remote) = *t.remote.lock().unwrap() {
                        // Echo this peer's latest send_ts so it can time the round
                        // trip; plain format when stats are off.
                        let timing = stats_on.then(|| Timing {
                            send_ts,
                            echo_ts: t.peer_send_ts.load(Ordering::Relaxed),
                        });
                        let n = voice_packet::encode(&mut packet, seq, timing, &payload[..pn]);
                        let _ = t.sock.send_to(&packet[..n], remote);
                    }
                }
                seq = seq.wrapping_add(1);
            }
        })?;

        Ok(Self {
            _capture: capture,
            playback,
            targets,
            peers: HashMap::new(),
            deaf,
            suspended: Arc::new(AtomicBool::new(false)),
            ns_wet,
            self_monitor,
            vad: vad_enabled,
            agc: agc_enabled,
            ec: ec_enabled,
            ec_strength: ec_strength_state,
            aec_method: aec_method_state,
            input_volume,
            jitter_ms: Arc::new(AtomicU32::new(DEFAULT_JITTER_MS)),
            epoch,
            next_source: 0,
        })
    }

    /// Toggle acoustic echo cancellation. Live.
    pub fn set_echo_cancel(&self, on: bool) {
        self.ec.store(on, Ordering::Relaxed);
    }

    /// Set residual-echo suppression strength (0–100). Applied on the next frame.
    pub fn set_echo_cancel_strength(&self, strength: u8) {
        self.ec_strength.store(strength as u32, Ordering::Relaxed);
    }

    /// Set mic input volume (0.0–1.0). Live, applied as a pre-amp on capture.
    pub fn set_input_volume(&self, v: f64) {
        self.input_volume.store((v as f32).to_bits(), Ordering::Relaxed);
    }

    /// Set master speaker volume (0.0–1.0). Live.
    pub fn set_output_volume(&self, v: f64) {
        self.playback.set_volume(v);
    }

    /// Switch the echo-canceller method. The capture thread rebuilds the canceller
    /// on the next frame.
    pub fn set_aec_method(&self, method: AecMethod) {
        self.aec_method.store(method_to_u8(method), Ordering::Relaxed);
    }

    /// Toggle earshot VAD (Voice-activity gating + speaking detection). Live.
    pub fn set_vad(&self, on: bool) {
        self.vad.store(on, Ordering::Relaxed);
    }

    /// Toggle AGC (auto gain). Live.
    pub fn set_agc(&self, on: bool) {
        self.agc.store(on, Ordering::Relaxed);
    }

    /// Set the RNNoise wet/dry mix in permille (0 = off, 1000 = full). Live.
    pub fn set_noise_wet(&self, wet: u32) {
        self.ns_wet.store(wet, Ordering::Relaxed);
    }

    /// Toggle the mic-test self-monitor (hear your own captured audio).
    pub fn set_self_monitor(&self, on: bool) {
        self.self_monitor.store(on, Ordering::Relaxed);
    }

    /// Set the receive jitter-buffer depth in milliseconds. Live — every peer's
    /// release loop re-reads it each frame, so the change applies mid-call.
    pub fn set_jitter_ms(&self, ms: u32) {
        self.jitter_ms.store(ms, Ordering::Relaxed);
    }

    /// Per-peer voice diagnostics (RTT, playout buffering, packet counters) for
    /// the stats readout. Cheap enough to poll ~once a second.
    pub fn stats_snapshot(&self) -> Vec<PeerStatsSnapshot> {
        let frame_ms = (FRAME_US / 1000) as u32; // 5 ms per frame

        self.peers
            .iter()
            .map(|(peer, p)| {
                let (counters, depth) = {
                    let jb = p.jitter.lock().unwrap();
                    (jb.counters(), jb.depth())
                };
                let rtt = p.rtt_ms.load(Ordering::Relaxed);
                let lane = self.playback.lane_samples(p.source_id) as u32;

                PeerStatsSnapshot {
                    peer: *peer,
                    rtt_ms: (rtt != 0).then_some(rtt),
                    jitter_ms: depth as u32 * frame_ms,
                    lane_ms: lane * 1000 / 48_000,
                    counters,
                }
            })
            .collect()
    }

    /// Add a peer: bind a recv socket, start its decode→playback lane, and return
    /// our `"ip:port"` to advertise. Idempotent-ish — a re-add replaces the peer.
    pub fn add_peer(&mut self, peer: Uuid) -> Result<String> {
        self.remove_peer(peer);

        let sock = Arc::new(UdpSocket::bind("0.0.0.0:0")?);
        sock.set_read_timeout(Some(Duration::from_millis(200)))?;
        let local_port = sock.local_addr()?.port();
        let target = Arc::new(SendTarget {
            sock: sock.clone(),
            remote: Mutex::new(None),
            peer_send_ts: AtomicU32::new(0),
        });

        let source_id = self.next_source;
        self.next_source += 1;
        let stop = Arc::new(AtomicBool::new(false));
        let rtt_ms = Arc::new(AtomicU32::new(0));

        // Reorder/dejitter buffer shared between the socket reader (fills it by
        // sequence) and the paced release loop (drains one frame per tick).
        let jitter = Arc::new(Mutex::new(JitterBuffer::new(ms_to_frames(
            self.jitter_ms.load(Ordering::Relaxed),
        ))));

        // Reader: parse each datagram, stamp it into the jitter buffer by sequence,
        // and (when timed) record the peer's send_ts to echo back and time the
        // round trip. No decode here — decoding rides the steady release clock, so
        // playback timing is independent of how unevenly packets land.
        let reader = {
            let jitter = jitter.clone();
            let stop = stop.clone();
            let target = target.clone();
            let rtt_ms = rtt_ms.clone();
            let epoch = self.epoch;
            std::thread::Builder::new()
                .name(format!("native-voice-rx-{source_id}"))
                .spawn(move || {
                    let mut buf = [0u8; 4000];
                    while !stop.load(Ordering::Relaxed) {
                        let n = match sock.recv(&mut buf) {
                            Ok(n) => n,
                            Err(_) => continue, // read timeout — re-check stop
                        };
                        let Some(pkt) = voice_packet::decode(&buf[..n]) else {
                            continue; // runt / unknown tag
                        };

                        if let Some(t) = pkt.timing {
                            // Echo this peer's send time in our outbound packets.
                            target.peer_send_ts.store(t.send_ts, Ordering::Relaxed);

                            // Our own send_ts came back as echo_ts → round trip.
                            // 0 means the peer hasn't echoed one yet.
                            if t.echo_ts != 0 {
                                let now = epoch.elapsed().as_millis() as u32;
                                let sample = now.wrapping_sub(t.echo_ts);
                                rtt_ms.store(smooth_rtt(rtt_ms.load(Ordering::Relaxed), sample), Ordering::Relaxed);
                            }
                        }

                        jitter.lock().unwrap().push(pkt.seq, pkt.payload);
                    }
                })?
        };

        // Release: keep the device's mixer lane topped up to a small cushion. The
        // render thread drains that lane at the true hardware clock, so refilling
        // it to a fixed depth paces decode to the device — no wall-clock timer to
        // drift against over a long call. Each refill pops the next frame in order
        // and decodes it (PLC on a concealed hole). A starved buffer stops the
        // refill; the mixer plays silence on underrun until packets resume.
        let release = {
            let jitter = jitter.clone();
            let jitter_ms = self.jitter_ms.clone();
            let playback = self.playback.clone();
            let deaf = self.deaf.clone();
            let suspended = self.suspended.clone();
            let stop = stop.clone();
            std::thread::Builder::new()
                .name(format!("native-voice-tx-{source_id}"))
                .spawn(move || {
                    // Hold 1 ms OS timer resolution for the call so the short
                    // LANE_POLL_MS sleeps below are honored (no Windows silence gaps).
                    let _timer_res = TimerResolutionGuard::new();

                    let mut dec = match Decoder::new(SampleRate::Hz48000, Channels::Mono) {
                        Ok(d) => d,
                        Err(e) => {
                            eprintln!("[native-voice] decoder init: {e}");
                            return;
                        }
                    };
                    let mut out = vec![0.0f32; FRAME];
                    let mut resampled: Vec<f32> = Vec::with_capacity(FRAME * 2);
                    let target_lane = TARGET_LANE_FRAMES * FRAME;
                    let poll = Duration::from_millis(LANE_POLL_MS);

                    // Clock-drift compensation: a steady sender/receiver sample-clock
                    // skew slowly fills or drains the buffer, and the dejitter buffer
                    // would otherwise drop a frame (or starve) every second or so —
                    // each one an audible click. Instead, hold the total buffered
                    // depth (device lane + jitter buffer) at a setpoint by nudging the
                    // playout speed ~1%, and varispeed-resample the decoded audio to
                    // match. Recomputed once per poll; the same speed resamples every
                    // frame decoded that tick.
                    let mut servo = DriftServo::new(ms_to_frames(DEFAULT_JITTER_MS) as f32);
                    let mut vari = Varispeed::new();
                    let mut speed = 1.0f32;

                    while !stop.load(Ordering::Relaxed) {
                        std::thread::sleep(poll);

                        let deaf_now =
                            deaf.load(Ordering::Relaxed) || suspended.load(Ordering::Relaxed);

                        // Servo the playout speed off the jitter-buffer depth — the
                        // one freely controllable cushion (the lane is pinned at its
                        // own target by the refill loop below, so it carries no drift
                        // signal). The setpoint is the buffer's prebuffer target, which
                        // it now actually holds. Held flat while deaf (nothing renders);
                        // resumes on unmute.
                        if !deaf_now {
                            let jb_frames = jitter.lock().unwrap().depth() as f32;

                            servo.set_target(ms_to_frames(jitter_ms.load(Ordering::Relaxed)) as f32);
                            speed = servo.observe(jb_frames);
                        }

                        loop {
                            // Stop once the lane holds its cushion. When deaf we
                            // skip the cushion check and drain the buffer (without
                            // rendering) to stay at the live edge, so unmuting
                            // resumes current audio rather than a stale backlog.
                            if !deaf_now && playback.lane_samples(source_id) >= target_lane {
                                break;
                            }

                            let popped = {
                                let mut jb = jitter.lock().unwrap();
                                jb.set_target(ms_to_frames(jitter_ms.load(Ordering::Relaxed)));
                                jb.pop()
                            };

                            // Decode in order (PLC on a concealed hole), then stretch
                            // the frame by the drift ratio before it reaches the lane.
                            let decoded = match popped {
                                JitterOut::Starve => break,
                                _ if deaf_now => continue,
                                JitterOut::Packet(p) => {
                                    dec.decode_float(Some(&p[..]), &mut out, false)
                                }
                                JitterOut::Conceal => {
                                    dec.decode_float(None::<&[u8]>, &mut out, false)
                                }
                            };

                            if let Ok(frames) = decoded {
                                resampled.clear();
                                vari.process(&out[..frames], speed, &mut resampled);
                                playback.push(source_id, &resampled);
                            }
                        }
                    }
                })?
        };

        self.targets.lock().unwrap().push(target.clone());
        self.peers.insert(
            peer,
            Peer { source_id, target, stop, handles: vec![reader, release], jitter, rtt_ms },
        );
        Ok(format!("{}:{}", crate::net::advertised_ip(), local_port))
    }

    /// Point our sender for `peer` at their `"ip:port"`.
    pub fn set_remote(&self, peer: Uuid, endpoint: &str) -> Result<()> {
        let addr: SocketAddr = endpoint
            .parse()
            .map_err(|e| anyhow!("bad voice endpoint {endpoint:?}: {e}"))?;
        if let Some(p) = self.peers.get(&peer) {
            *p.target.remote.lock().unwrap() = Some(addr);
        }
        Ok(())
    }

    /// Tear down one peer's transport and mixer lane.
    pub fn remove_peer(&mut self, peer: Uuid) {
        if let Some(mut p) = self.peers.remove(&peer) {
            p.stop.store(true, Ordering::Relaxed);
            self.targets
                .lock()
                .unwrap()
                .retain(|t| !Arc::ptr_eq(t, &p.target));
            for h in p.handles.drain(..) {
                let _ = h.join();
            }
            self.playback.remove_source(p.source_id);
        }
    }

    /// Deafen: stop pushing received audio to playback.
    pub fn set_deaf(&self, on: bool) {
        self.deaf.store(on, Ordering::Relaxed);
    }

    /// Temporary output silence (e.g. Settings open), independent of deafen.
    pub fn set_suspended(&self, on: bool) {
        self.suspended.store(on, Ordering::Relaxed);
    }

    pub fn is_empty(&self) -> bool {
        self.peers.is_empty()
    }
}

/// Closed-gate gain floor (~-54 dBFS) — the gate's "range". Resting at a faint
/// trace of room tone instead of slamming to 0 avoids the unnatural "dead air"
/// drop and shrinks the gain step at the edge; quiet enough to be inaudible.
pub(crate) const FLOOR_GAIN: f32 = 0.002;

/// Gate gain envelope: a per-sample gain (`out = env*in`). `env` tracks `target`
/// (1.0 open, [`FLOOR_GAIN`] closed) via a one-pole filter — fast attack, slow
/// exponential release. The exponential trajectory has no derivative "corner", so
/// neither edge inserts the broadband step a sharper gain change clicks on. `env`
/// settles exactly on `target`, so steady speech passes at unity (the envelope
/// only works at the transitions) and a closed gate rests at the floor.
pub(crate) fn ramp_gain(buf: &mut [f32], env: &mut f32, target: f32) {
    // One-pole coefficients, per sample @ 48 kHz: fast attack (τ ~3 ms), slow
    // exponential release (τ ~35 ms, ~140 ms tail) for a smooth, click-free decay.
    const ATTACK_COEF: f32 = 0.007;
    const RELEASE_COEF: f32 = 0.0006;
    for s in buf.iter_mut() {
        let coef = if target > *env { ATTACK_COEF } else { RELEASE_COEF };
        *env += coef * (target - *env);
        if (target - *env).abs() < 1.0e-4 {
            *env = target; // settle exactly: unity when open, floor when closed
        }
        *s *= *env;
    }
}

/// Raises the OS multimedia-timer resolution to 1 ms for its lifetime, then
/// restores it on drop. The receive release loop sleeps in `LANE_POLL_MS` (3 ms)
/// steps; on stock Windows a plain `sleep` rounds up to the ~15.6 ms default timer
/// tick, draining the playback lane to silence. Holding 1 ms resolution while a
/// call runs makes the short sleeps accurate. No-op off Windows.
struct TimerResolutionGuard;

impl TimerResolutionGuard {
    fn new() -> TimerResolutionGuard {
        #[cfg(target_os = "windows")]
        // SAFETY: `timeBeginPeriod` is always safe to call; it is paired with the
        // matching `timeEndPeriod` in `Drop` (the API is reference-counted).
        unsafe {
            windows::Win32::Media::timeBeginPeriod(1);
        }

        TimerResolutionGuard
    }
}

impl Drop for TimerResolutionGuard {
    fn drop(&mut self) {
        #[cfg(target_os = "windows")]
        // SAFETY: balances the `timeBeginPeriod(1)` from `new`.
        unsafe {
            windows::Win32::Media::timeEndPeriod(1);
        }
    }
}

/// Convert a jitter-buffer depth in milliseconds to whole 5 ms frames, clamped
/// to at least one frame so the buffer always holds back a packet to reorder on.
fn ms_to_frames(ms: u32) -> usize {
    ((ms as u64 * 1000 / FRAME_US) as usize).max(1)
}

/// Implausibly large RTT sample (ms) — a stale echo or wrapped timestamp. Ignored
/// so one bad sample can't poison the smoothed value.
const RTT_OUTLIER_MS: u32 = 10_000;

/// Smooth a new RTT sample into the running estimate. Seeds on the first sample
/// (`prev == 0`), then a 1/8-weight EMA; outliers are dropped.
fn smooth_rtt(prev: u32, sample: u32) -> u32 {
    if sample > RTT_OUTLIER_MS {
        return prev;
    }

    if prev == 0 {
        return sample;
    }

    prev - prev / 8 + sample / 8
}

#[cfg(test)]
mod jitter_depth_tests {
    use super::{ms_to_frames, smooth_rtt};

    #[test]
    fn converts_ms_to_whole_frames_with_a_one_frame_floor() {
        assert_eq!(ms_to_frames(20), 4); // 20 ms / 5 ms
        assert_eq!(ms_to_frames(5), 1);
        assert_eq!(ms_to_frames(0), 1); // never zero — always hold one frame
        assert_eq!(ms_to_frames(100), 20);
    }

    #[test]
    fn smooth_rtt_seeds_then_eases_toward_new_samples() {
        assert_eq!(smooth_rtt(0, 40), 40, "first sample seeds the estimate");

        let r = smooth_rtt(40, 80);
        assert!(r > 40 && r < 80, "eases toward the new sample, got {r}");
    }

    #[test]
    fn smooth_rtt_ignores_outliers() {
        assert_eq!(smooth_rtt(40, 50_000), 40, "a stale/wrapped sample is dropped");
    }
}

fn rms_dbfs(frame: &[f32]) -> f32 {
    if frame.is_empty() {
        return -120.0;
    }
    let sum: f32 = frame.iter().map(|s| s * s).sum();
    let rms = (sum / frame.len() as f32).sqrt();
    if rms <= 1e-7 {
        -120.0
    } else {
        20.0 * rms.log10()
    }
}
