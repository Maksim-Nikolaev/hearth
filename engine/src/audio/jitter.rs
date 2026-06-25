//! Receive-side dejitter / reorder buffer for the native voice transport.
//!
//! The transport stamps every Opus packet with a `u16` sequence (see
//! `native_voice`). Packets cross a real network, so they arrive unevenly,
//! reordered, or not at all. Decoding each one the instant it lands makes the
//! playback lane underrun on every late packet — an audible click/boop.
//!
//! This buffer decouples arrival from playback: [`push`](JitterBuffer::push)
//! deposits packets keyed by sequence; a steady-cadence consumer calls
//! [`pop`](JitterBuffer::pop) once per frame to drain them *in order*. It holds
//! back `target_frames` of audio before the first release (the prebuffer depth)
//! so later jitter is absorbed without starving the lane.

use std::collections::HashMap;

/// Longest run of consecutive concealed (PLC) frames before the buffer gives up
/// on the hole and resyncs to the earliest buffered frame. ~40 ms at the 5 ms
/// native framing — beyond this, PLC is audible garbage and skipping is cleaner.
const MAX_CONCEAL_FRAMES: u16 = 8;

/// One frame's worth of release decision from [`JitterBuffer::pop`].
pub(crate) enum JitterOut {
    /// Decode and play this Opus payload.
    Packet(Vec<u8>),
    /// The expected sequence is genuinely missing — run Opus PLC for one frame.
    Conceal,
    /// Nothing to release yet (still prebuffering, or a true underrun). The
    /// caller plays silence for this frame.
    Starve,
}

/// Running packet accounting for the receive path, the single source of truth
/// for the voice-stats readout. Concealed ≈ frames that never arrived (loss);
/// `late_dropped` and `overfill_dropped` are arrivals discarded by the buffer.
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct JitterCounters {
    /// Packets buffered for playout (in-order or reordered, not late).
    pub accepted: u64,
    /// PLC frames emitted for missing sequences (network loss).
    pub concealed: u64,
    /// Arrivals dropped because their slot had already played.
    pub late_dropped: u64,
    /// Oldest frames dropped to bound depth (burst / faster sender clock).
    pub overfill_dropped: u64,
    /// Long-gap jumps to the live edge.
    pub resynced: u64,
}

pub(crate) struct JitterBuffer {
    /// Prebuffer depth: hold this many frames before the first release.
    target: usize,
    buf: HashMap<u16, Vec<u8>>,
    /// Sequence to release next; `None` until the first packet sets the origin.
    next_seq: Option<u16>,
    /// Latches true once `target` is first reached, then stays true.
    playing: bool,
    counters: JitterCounters,
}

impl JitterBuffer {
    pub fn new(target_frames: usize) -> JitterBuffer {
        JitterBuffer {
            target: target_frames.max(1),
            buf: HashMap::new(),
            next_seq: None,
            playing: false,
            counters: JitterCounters::default(),
        }
    }

    /// Snapshot the running packet accounting (for the voice-stats readout).
    pub fn counters(&self) -> JitterCounters {
        self.counters
    }

    /// Frames currently buffered awaiting release (the live playout depth).
    pub fn depth(&self) -> usize {
        self.buf.len()
    }

    /// Change the prebuffer depth live (e.g. the user moved the jitter slider).
    /// Dropping the play latch re-runs the prebuffer at the new depth: a deeper
    /// buffer holds back until the larger cushion refills (inserting the extra
    /// latency), a shallower one re-latches as soon as the next frame is ready.
    pub fn set_target(&mut self, target_frames: usize) {
        let target = target_frames.max(1);
        if target != self.target {
            self.target = target;
            self.playing = false;
        }
    }

    pub fn push(&mut self, seq: u16, payload: &[u8]) {
        match self.next_seq {
            // The first packet sets the release origin.
            None => self.next_seq = Some(seq),
            // Discard anything whose slot has already been released (wrapping
            // signed distance < 0), so a late straggler can't replay or be
            // misread as a future frame.
            Some(next) if (seq.wrapping_sub(next) as i16) < 0 => {
                self.counters.late_dropped += 1;
                return;
            }
            Some(_) => {}
        }

        self.buf.insert(seq, payload.to_vec());
        self.counters.accepted += 1;

        // Bound the depth so a burst or a faster-than-us sender clock can't grow
        // playout latency without limit over a long call. Drop the oldest frames
        // (closest to the play cursor) and skip the cursor past them.
        let cap = (self.target * 2).max(self.target + 1);
        while self.buf.len() > cap {
            let Some(next) = self.next_seq else { break };
            let oldest = *self.buf.keys().min_by_key(|k| k.wrapping_sub(next)).unwrap();
            self.buf.remove(&oldest);
            self.next_seq = Some(oldest.wrapping_add(1));
            self.counters.overfill_dropped += 1;
        }
    }

    pub fn pop(&mut self) -> JitterOut {
        if !self.playing {
            if self.buf.len() >= self.target {
                self.playing = true;
            } else {
                return JitterOut::Starve;
            }
        }

        let Some(seq) = self.next_seq else {
            return JitterOut::Starve;
        };

        if let Some(payload) = self.buf.remove(&seq) {
            self.next_seq = Some(seq.wrapping_add(1));

            return JitterOut::Packet(payload);
        }

        // The expected frame is absent. An empty buffer is just an underrun:
        // hold the slot and play silence until packets resume.
        if self.buf.is_empty() {
            return JitterOut::Starve;
        }

        // Later frames are waiting, so there's a real hole. Conceal small holes
        // one frame at a time (PLC). For a large hole — a long stall where the
        // missing run is unrecoverable — concealing every frame would emit a
        // long burst of PLC garbage, so jump straight to the earliest buffered
        // frame (the live edge) and drop the dead span.
        let earliest = *self.buf.keys().min_by_key(|k| k.wrapping_sub(seq)).unwrap();
        let hole = earliest.wrapping_sub(seq);

        if hole > MAX_CONCEAL_FRAMES {
            let payload = self.buf.remove(&earliest).unwrap();
            self.next_seq = Some(earliest.wrapping_add(1));
            self.counters.resynced += 1;

            return JitterOut::Packet(payload);
        }

        self.next_seq = Some(seq.wrapping_add(1));
        self.counters.concealed += 1;

        JitterOut::Conceal
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn payload(out: JitterOut) -> Vec<u8> {
        match out {
            JitterOut::Packet(p) => p,
            JitterOut::Conceal => panic!("expected Packet, got Conceal"),
            JitterOut::Starve => panic!("expected Packet, got Starve"),
        }
    }

    #[test]
    fn prebuffers_then_releases_in_order() {
        let mut jb = JitterBuffer::new(3);
        jb.push(0, &[10]);
        jb.push(1, &[11]);

        // Below the target depth: still prebuffering, nothing released yet.
        assert!(matches!(jb.pop(), JitterOut::Starve));

        jb.push(2, &[12]);

        // Target reached → release every buffered frame in sequence order.
        assert_eq!(payload(jb.pop()), [10]);
        assert_eq!(payload(jb.pop()), [11]);
        assert_eq!(payload(jb.pop()), [12]);

        // Drained: back to starving.
        assert!(matches!(jb.pop(), JitterOut::Starve));
    }

    #[test]
    fn reorders_out_of_order_arrivals() {
        let mut jb = JitterBuffer::new(3);
        jb.push(0, &[10]);
        jb.push(2, &[12]);
        jb.push(1, &[11]);

        assert_eq!(payload(jb.pop()), [10]);
        assert_eq!(payload(jb.pop()), [11]);
        assert_eq!(payload(jb.pop()), [12]);
    }

    #[test]
    fn conceals_lost_frame_then_continues() {
        let mut jb = JitterBuffer::new(3);
        jb.push(0, &[10]);
        jb.push(1, &[11]);
        jb.push(3, &[13]); // seq 2 never arrives

        assert_eq!(payload(jb.pop()), [10]);
        assert_eq!(payload(jb.pop()), [11]);

        // The hole at seq 2 conceals (PLC) rather than stalling on it, and the
        // already-buffered seq 3 still plays afterward.
        assert!(matches!(jb.pop(), JitterOut::Conceal));
        assert_eq!(payload(jb.pop()), [13]);
    }

    #[test]
    fn drops_packets_already_played() {
        let mut jb = JitterBuffer::new(1);
        jb.push(5, &[5]);
        assert_eq!(payload(jb.pop()), [5]); // next expected is now 6

        // A straggler for an already-played sequence arrives far too late.
        jb.push(2, &[2]);

        // It is discarded — not buffered, not replayed, and it must not be
        // mistaken for a later frame that would trigger a spurious conceal.
        assert!(matches!(jb.pop(), JitterOut::Starve));
    }

    #[test]
    fn releases_in_order_across_u16_wraparound() {
        let mut jb = JitterBuffer::new(3);
        jb.push(65534, &[1]);
        jb.push(65535, &[2]);
        jb.push(0, &[3]);

        assert_eq!(payload(jb.pop()), [1]);
        assert_eq!(payload(jb.pop()), [2]);
        assert_eq!(payload(jb.pop()), [3]);
    }

    #[test]
    fn resyncs_after_a_long_gap_instead_of_concealing_forever() {
        let mut jb = JitterBuffer::new(1);
        jb.push(0, &[0]);
        assert_eq!(payload(jb.pop()), [0]); // next expected is now 1

        // A long stall: sequences 1..49 are lost, the stream resumes at 50.
        // Concealing the whole hole would emit ~49 PLC frames of garbage, so the
        // buffer jumps to the live edge after a bounded number of conceals.
        jb.push(50, &[50]);

        let mut conceals = 0;
        loop {
            match jb.pop() {
                JitterOut::Packet(p) => {
                    assert_eq!(p, [50]);
                    break;
                }
                JitterOut::Conceal => {
                    conceals += 1;
                    assert!(conceals <= 8, "concealed too long instead of resyncing");
                }
                JitterOut::Starve => panic!("unexpected starve while a frame is buffered"),
            }
        }
    }

    #[test]
    fn raising_target_reprebuffers_for_more_cushion() {
        let mut jb = JitterBuffer::new(1);
        jb.push(0, &[0]);
        assert_eq!(payload(jb.pop()), [0]); // playing, next expected is 1

        // Deepening the buffer mid-stream holds playback back until the larger
        // cushion refills, so the extra latency is actually inserted.
        jb.set_target(3);
        jb.push(1, &[1]);
        assert!(matches!(jb.pop(), JitterOut::Starve), "only 1 of 3 frames buffered");

        jb.push(2, &[2]);
        jb.push(3, &[3]);
        assert_eq!(payload(jb.pop()), [1]); // cushion full → resume in order
    }

    #[test]
    fn caps_depth_and_skips_to_live_edge_when_overfilled() {
        // A burst, or a sender clock running faster than our playback, fills the
        // buffer faster than it drains. Depth must stay bounded (no creeping
        // latency over a long call), dropping the oldest and skipping ahead.
        let mut jb = JitterBuffer::new(2); // target 2 → cap 4
        for s in 0..100u16 {
            jb.push(s, &[s as u8]);
        }

        assert!(jb.buf.len() <= 4, "depth must stay bounded, got {}", jb.buf.len());

        // Playback resumes at the live edge, not the stale seq 0.
        assert!(payload(jb.pop())[0] >= 96, "skipped the stale backlog to the live edge");
    }

    #[test]
    fn counts_accepted_concealed_late_and_resync() {
        let mut jb = JitterBuffer::new(2);
        jb.push(0, &[0]); // accepted
        jb.push(1, &[1]); // accepted
        jb.push(3, &[3]); // accepted (seq 2 will be a hole)

        assert_eq!(payload(jb.pop()), [0]);
        assert_eq!(payload(jb.pop()), [1]);
        assert!(matches!(jb.pop(), JitterOut::Conceal)); // seq 2 hole
        assert_eq!(payload(jb.pop()), [3]);

        jb.push(1, &[1]); // already played → late drop

        let c = jb.counters();
        assert_eq!(c.accepted, 3, "three in-order arrivals buffered");
        assert_eq!(c.concealed, 1, "one PLC frame for the seq-2 hole");
        assert_eq!(c.late_dropped, 1, "one straggler discarded");
        assert_eq!(c.resynced, 0, "no long-gap resync here");
    }
}
