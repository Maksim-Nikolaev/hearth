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
use audiopus::{coder::Decoder, coder::Encoder, Application, Channels, SampleRate};
use std::collections::HashMap;
use std::net::{SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::mpsc::UnboundedSender;
use uuid::Uuid;

/// Opus frame: 5 ms @ 48 kHz mono. Smaller frame = less packetization delay and
/// a shallower playback lane (one frame) than the 10 ms default.
const FRAME: usize = 240;

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
    ) -> Result<Self> {
        eprintln!("[native-voice] active — WASAPI IAudioClient3 capture/playback + Opus + UDP");
        let playback = Arc::new(NativePlayback::start(output_device)?);
        let targets: Arc<Mutex<Vec<Arc<SendTarget>>>> = Arc::new(Mutex::new(Vec::new()));
        let deaf = Arc::new(AtomicBool::new(false));

        let encoder = Encoder::new(SampleRate::Hz48000, Channels::Mono, Application::LowDelay)?;
        let mut acc: Vec<f32> = Vec::with_capacity(FRAME * 2);
        // [seq: u16 BE | opus payload]. The 2-byte sequence lets the receiver
        // detect loss and run Opus PLC, and drop late/duplicate packets.
        let mut packet = vec![0u8; 4000];
        let mut seq: u16 = 0;
        let silence = [0.0f32; FRAME];
        let targets_cb = targets.clone();

        let capture = NativeCapture::start(input_device, move |mono| {
            acc.extend_from_slice(mono);
            while acc.len() >= FRAME {
                let frame: Vec<f32> = acc.drain(..FRAME).collect();
                let rms_db = rms_dbfs(&frame);
                let open = {
                    let mut g = gate.lock().unwrap();
                    g.update_level(rms_db, rms_db > -60.0);
                    g.open()
                };
                let _ = evt_tx.send(SessionEvent::InputLevel(rms_db));

                let pcm: &[f32] = if open { &frame } else { &silence };
                packet[..2].copy_from_slice(&seq.to_be_bytes());
                let n = match encoder.encode_float(pcm, &mut packet[2..]) {
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
            next_source: 0,
        })
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
                    let deaf_now = deaf.load(Ordering::Relaxed);

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
