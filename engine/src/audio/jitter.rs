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
//! back `target` frames of audio before the first release (the prebuffer depth)
//! so later jitter is absorbed without starving the lane.
//!
//! The buffer does **not** regulate its own steady-state depth. A separate
//! clock-drift servo ([`super::drift`]) owns that, nudging playout speed to hold
//! the live depth near `target`. So overflow here is a pure safety net: it fires
//! only on a burst large enough to clear `target + SAFETY_MARGIN_FRAMES`, far
//! above where the servo holds the depth, so it effectively never trips in
//! normal operation.

use std::collections::HashMap;

/// Longest run of consecutive concealed (PLC) frames before the buffer gives up
/// on the hole and resyncs to the earliest buffered frame. ~40 ms at the 5 ms
/// native framing — beyond this, PLC is audible garbage and skipping is cleaner.
const MAX_CONCEAL_FRAMES: u16 = 8;

/// Headroom above `target` before the safety net drops the oldest frame. Sized to
/// swallow a full arrival burst: the sender hands the receiver a whole capture
/// quantum's worth of frames back-to-back, and two coalescing quanta can briefly
/// stack ~60 ms on top of the depth the drift servo holds. ~60 ms here (12 frames
/// at the 5 ms native framing) absorbs that. It is a *ceiling*, not a target — the
/// servo keeps the steady depth at `target`, so a wider cap costs no latency and
/// only stops bursts from clipping; only a genuine overrun ever reaches it.
const SAFETY_MARGIN_FRAMES: usize = 12;

/// Smallest meaningful prebuffer/target — one frame is just a reorder slot.
const MIN_TARGET_FRAMES: usize = 1;

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
    /// Oldest frames the safety net dropped to bound depth (real overrun only).
    pub overfill_dropped: u64,
    /// Long-gap jumps to the live edge.
    pub resynced: u64,
}

pub(crate) struct JitterBuffer {
    /// Prebuffer depth (frames) and the depth the drift servo aims to hold. Set
    /// live from the user's jitter slider; not self-adjusted by this buffer.
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
            target: target_frames.max(MIN_TARGET_FRAMES),
            buf: HashMap::new(),
            next_seq: None,
            playing: false,
            counters: JitterCounters::default(),
        }
    }

    /// Current prebuffer / servo-target depth in frames.
    #[cfg(test)]
    pub fn current_target(&self) -> usize {
        self.target
    }

    /// Snapshot the running packet accounting (for the voice-stats readout).
    pub fn counters(&self) -> JitterCounters {
        self.counters
    }

    /// Frames currently buffered awaiting release (the live playout depth).
    pub fn depth(&self) -> usize {
        self.buf.len()
    }

    /// Set the prebuffer / servo-target depth live (the user moved the jitter
    /// slider). The safety cap tracks it at `target + SAFETY_MARGIN_FRAMES`.
    pub fn set_target(&mut self, target_frames: usize) {
        self.target = target_frames.max(MIN_TARGET_FRAMES);
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

        // Safety net only: bound depth to `target + SAFETY_MARGIN_FRAMES`. The
        // drift servo holds the steady depth near `target`, well under this cap,
        // so this drops a frame only on a genuine overrun. Drop the oldest
        // (closest to the play cursor) and skip the cursor past it.
        let cap = self.target + SAFETY_MARGIN_FRAMES;
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

        // The expected frame is absent. An empty buffer is a genuine underrun:
        // play silence, and drop the play latch so the next fills re-prebuffer to
        // `target` — rebuilding the cushion only after a real gap, when the audio
        // is already interrupted.
        if self.buf.is_empty() {
            self.playing = false;

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
    fn prebuffers_to_target_then_releases_in_order() {
        // Holds back `target` frames before the first release, then drains in
        // order. With target 2, one frame isn't enough to start.
        let mut jb = JitterBuffer::new(2);
        jb.push(0, &[10]);
        assert!(matches!(jb.pop(), JitterOut::Starve), "one frame under target prebuffers");

        jb.push(1, &[11]);
        assert_eq!(payload(jb.pop()), [10], "target reached, release begins");
        assert_eq!(payload(jb.pop()), [11]);

        assert!(matches!(jb.pop(), JitterOut::Starve));
    }

    #[test]
    fn reorders_out_of_order_arrivals() {
        let mut jb = JitterBuffer::new(1);
        jb.push(0, &[10]);
        jb.push(2, &[12]);
        jb.push(1, &[11]);

        assert_eq!(payload(jb.pop()), [10]);
        assert_eq!(payload(jb.pop()), [11]);
        assert_eq!(payload(jb.pop()), [12]);
    }

    #[test]
    fn conceals_lost_frame_then_continues() {
        let mut jb = JitterBuffer::new(1);
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
        assert_eq!(jb.counters().late_dropped, 1);
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
        assert_eq!(jb.counters().resynced, 1);
    }

    #[test]
    fn target_is_fixed_not_self_adjusted() {
        // Regression: the buffer must not grow/decay its own target on jitter —
        // the drift servo owns steady-state depth now. Holes and late arrivals
        // leave the target untouched.
        let mut jb = JitterBuffer::new(2);
        assert_eq!(jb.current_target(), 2);

        jb.push(0, &[0]);
        jb.push(1, &[1]);
        jb.push(3, &[3]); // a hole at seq 2
        assert_eq!(payload(jb.pop()), [0]);
        assert_eq!(payload(jb.pop()), [1]);
        assert!(matches!(jb.pop(), JitterOut::Conceal));

        assert_eq!(jb.current_target(), 2, "target unchanged by jitter");
    }

    #[test]
    fn set_target_moves_the_prebuffer_live() {
        let mut jb = JitterBuffer::new(2);
        jb.set_target(4);
        assert_eq!(jb.current_target(), 4);

        // Three frames is now under target, so it still prebuffers.
        jb.push(0, &[0]);
        jb.push(1, &[1]);
        jb.push(2, &[2]);
        assert!(matches!(jb.pop(), JitterOut::Starve), "under the raised target");

        jb.push(3, &[3]);
        assert_eq!(payload(jb.pop()), [0], "target reached, release begins");
    }

    #[test]
    fn safety_net_bounds_depth_only_on_real_overrun() {
        // A burst within `target + SAFETY_MARGIN_FRAMES` is fully absorbed — no
        // drop. Only a burst past the cap trims the oldest and skips ahead.
        let target = 2usize;
        let cap = target + SAFETY_MARGIN_FRAMES;
        let mut jb = JitterBuffer::new(target);

        for s in 0..cap as u16 {
            jb.push(s, &[s as u8]);
        }
        assert_eq!(jb.counters().overfill_dropped, 0, "a burst up to the cap is absorbed");
        assert_eq!(jb.depth(), cap);

        // Two more push past the cap: the oldest are dropped, depth pinned at cap.
        jb.push(cap as u16, &[0]);
        jb.push(cap as u16 + 1, &[0]);
        assert_eq!(jb.depth(), cap, "depth bounded at the cap");
        assert_eq!(jb.counters().overfill_dropped, 2);

        // Playback resumes at the live edge, not the stale dropped frames.
        assert!(payload(jb.pop())[0] >= 2, "skipped the dropped backlog");
    }

    #[test]
    fn counts_accepted_concealed_late_and_resync() {
        let mut jb = JitterBuffer::new(1);
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
