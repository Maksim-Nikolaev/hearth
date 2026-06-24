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

use super::gate::Gate;
use super::native::{NativeCapture, NativePlayback};
use crate::session::SessionEvent;
use anyhow::{anyhow, Result};
use aec_rs::Aec;
use audiopus::{coder::Decoder, coder::Encoder, Application, Channels, SampleRate};
use earshot::{VoiceActivityDetector, VoiceActivityProfile};
use nnnoiseless::DenoiseState;

/// speexdsp's `Aec` holds raw pointers; it lives entirely on the capture thread,
/// so wrap it to move into that thread's closure. The method (rather than field
/// access) keeps closures capturing the whole wrapper, not the inner `Aec`.
struct SendAec(Aec);
unsafe impl Send for SendAec {}
impl SendAec {
    fn cancel(&mut self, mic: &[i16], far: &[i16], out: &mut [i16]) {
        self.0.cancel_echo(mic, far, out);
    }
}

/// AEC frame (10 ms) and adaptive-filter tail (~100 ms of room echo) at 48 kHz.
const AEC_FRAME: usize = 480;
const AEC_FILTER_LEN: i32 = 4800;
use std::collections::HashMap;
use std::net::{SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::mpsc::UnboundedSender;
use uuid::Uuid;

/// Opus frame: 5 ms @ 48 kHz mono. Smaller frame = less packetization delay and
/// a shallower playback lane (one frame) than the 10 ms default.
const FRAME: usize = 240;

/// RNNoise frame: 10 ms @ 48 kHz. NS buffers this much (so enabling it adds
/// ~10 ms of latency over the 5 ms Opus framing).
const NS_FRAME: usize = DenoiseState::FRAME_SIZE; // 480

/// A send destination, shared between the capture/encode thread and the session.
/// The remote is filled in once the peer's endpoint arrives over signaling.
struct SendTarget {
    sock: Arc<UdpSocket>,
    remote: Mutex<Option<SocketAddr>>,
}

struct Peer {
    source_id: u64,
    target: Arc<SendTarget>,
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
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
        voice_status: crate::session::VoiceStatus,
    ) -> Result<Self> {
        eprintln!("[native-voice] active — WASAPI IAudioClient3 capture/playback + Opus + UDP");
        let playback = Arc::new(NativePlayback::start(output_device)?);
        let targets: Arc<Mutex<Vec<Arc<SendTarget>>>> = Arc::new(Mutex::new(Vec::new()));
        let deaf = Arc::new(AtomicBool::new(false));
        let ns_wet = Arc::new(AtomicU32::new(ns_wet));

        let encoder = Encoder::new(SampleRate::Hz48000, Channels::Mono, Application::LowDelay)?;
        let mut acc: Vec<f32> = Vec::with_capacity(FRAME * 2);
        // [seq: u16 BE | opus payload]. The 2-byte sequence lets the receiver
        // detect loss and run Opus PLC, and drop late/duplicate packets.
        let mut packet = vec![0u8; 4000];
        let mut seq: u16 = 0;
        let targets_cb = targets.clone();

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
        let mut aec = SendAec(Aec::new(AEC_FRAME, AEC_FILTER_LEN, 48000));
        let mut aec_in: Vec<f32> = Vec::with_capacity(AEC_FRAME * 2);

        eprintln!(
            "[native-voice] filters: ns_wet={} vad={vad} agc={agc} ec={ec}",
            ns_wet.load(Ordering::Relaxed)
        );

        let capture = NativeCapture::start(input_device, move |mono| {
            // AEC first (on the raw mic), so NS/AGC see echo-free audio. When off,
            // `src` is the mic untouched.
            let ec_on = ec_cb.load(Ordering::Relaxed);
            let cleaned: Vec<f32> = if ec_on {
                aec_in.extend_from_slice(mono);
                let mut out = Vec::with_capacity(aec_in.len());
                while aec_in.len() >= AEC_FRAME {
                    let mic_f: Vec<f32> = aec_in.drain(..AEC_FRAME).collect();
                    let mut far = [0i16; AEC_FRAME];
                    {
                        let mut fr = far_ring.lock().unwrap();
                        for d in far.iter_mut() {
                            *d = fr.pop_front().map_or(0, |v: f32| (v.clamp(-1.0, 1.0) * 32767.0) as i16);
                        }
                    }
                    let mic_i: Vec<i16> =
                        mic_f.iter().map(|s| (s.clamp(-1.0, 1.0) * 32767.0) as i16).collect();
                    let mut out_i = [0i16; AEC_FRAME];
                    aec.cancel(&mic_i, &far, &mut out_i);
                    out.extend(out_i.iter().map(|s| *s as f32 / 32768.0));
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
                        *s = (*s * agc_gain).clamp(-1.0, 1.0);
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
                    ramp_gain(&mut mon, &mut mon_gain, if mon_open { 1.0 } else { 0.0 });
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
                // the (faded) frame — silence once fully closed.
                ramp_gain(&mut frame, &mut send_gain, if open { 1.0 } else { 0.0 });
                packet[..2].copy_from_slice(&seq.to_be_bytes());
                let n = match encoder.encode_float(&frame, &mut packet[2..]) {
                    Ok(n) => n,
                    Err(_) => continue,
                };
                seq = seq.wrapping_add(1);
                for t in targets_cb.lock().unwrap().iter() {
                    if let Some(remote) = *t.remote.lock().unwrap() {
                        let _ = t.sock.send_to(&packet[..2 + n], remote);
                    }
                }
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
            next_source: 0,
        })
    }

    /// Toggle acoustic echo cancellation. Live.
    pub fn set_echo_cancel(&self, on: bool) {
        self.ec.store(on, Ordering::Relaxed);
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

    /// Add a peer: bind a recv socket, start its decode→playback lane, and return
    /// our `"ip:port"` to advertise. Idempotent-ish — a re-add replaces the peer.
    pub fn add_peer(&mut self, peer: Uuid) -> Result<String> {
        self.remove_peer(peer);

        let sock = Arc::new(UdpSocket::bind("0.0.0.0:0")?);
        sock.set_read_timeout(Some(Duration::from_millis(200)))?;
        let local_port = sock.local_addr()?.port();
        let target = Arc::new(SendTarget { sock: sock.clone(), remote: Mutex::new(None) });

        let source_id = self.next_source;
        self.next_source += 1;
        let stop = Arc::new(AtomicBool::new(false));

        let playback = self.playback.clone();
        let deaf = self.deaf.clone();
        let suspended = self.suspended.clone();
        let stop_thread = stop.clone();
        let handle = std::thread::Builder::new()
            .name(format!("native-voice-rx-{source_id}"))
            .spawn(move || {
                let mut dec = match Decoder::new(SampleRate::Hz48000, Channels::Mono) {
                    Ok(d) => d,
                    Err(e) => {
                        eprintln!("[native-voice] decoder init: {e}");
                        return;
                    }
                };
                let mut buf = [0u8; 4000];
                let mut out = vec![0.0f32; FRAME];
                let mut expected: Option<u16> = None;
                while !stop_thread.load(Ordering::Relaxed) {
                    let n = match sock.recv(&mut buf) {
                        Ok(n) if n >= 3 => n,
                        Ok(_) => continue,    // runt packet
                        Err(_) => continue,   // read timeout — re-check stop
                    };
                    let seq = u16::from_be_bytes([buf[0], buf[1]]);
                    let payload = &buf[2..n];
                    let deaf_now = deaf.load(Ordering::Relaxed) || suspended.load(Ordering::Relaxed);

                    if let Some(exp) = expected {
                        let gap = seq.wrapping_sub(exp) as i16;
                        if gap < 0 {
                            continue; // late or duplicate — drop
                        }
                        // Conceal up to a bounded run of lost frames with Opus PLC.
                        for _ in 0..(gap as usize).min(10) {
                            if let Ok(frames) = dec.decode_float(None::<&[u8]>, &mut out, false) {
                                if !deaf_now {
                                    playback.push(source_id, &out[..frames]);
                                }
                            }
                        }
                    }

                    if let Ok(frames) = dec.decode_float(Some(payload), &mut out, false) {
                        if !deaf_now {
                            playback.push(source_id, &out[..frames]);
                        }
                    }
                    expected = Some(seq.wrapping_add(1));
                }
            })?;

        self.targets.lock().unwrap().push(target.clone());
        self.peers.insert(peer, Peer { source_id, target, stop, handle: Some(handle) });
        Ok(format!("{}:{}", local_ip(), local_port))
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
            if let Some(h) = p.handle.take() {
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

/// Best-effort LAN IP (UDP-connect trick, no packet sent); loopback fallback.
fn local_ip() -> String {
    std::net::UdpSocket::bind("0.0.0.0:0")
        .and_then(|s| {
            s.connect("8.8.8.8:80")?;
            s.local_addr()
        })
        .map(|a| a.ip().to_string())
        .unwrap_or_else(|_| "127.0.0.1".to_string())
}

/// Apply a per-sample gain to `buf`, ramping `gain` toward `target` (~10 ms full
/// 0↔1 swing at 48 kHz) so the gate opens/closes without a click.
pub(crate) fn ramp_gain(buf: &mut [f32], gain: &mut f32, target: f32) {
    const STEP: f32 = 1.0 / 480.0;
    for s in buf.iter_mut() {
        if *gain < target {
            *gain = (*gain + STEP).min(target);
        } else if *gain > target {
            *gain = (*gain - STEP).max(target);
        }
        *s *= *gain;
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
