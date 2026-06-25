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

/// Adaptive prebuffer floor — the depth held on a clean link, for minimal
/// latency. One frame (~5 ms) is just a reorder slot.
const FLOOR_FRAMES: usize = 1;

/// Frames added to the adaptive target on each observed jitter event (a hole, a
/// late arrival, or a resync), up to the ceiling.
const GROW_FRAMES: usize = 2;

/// Clean releases between each one-frame decay back toward the floor (~1 s at
/// the 5 ms native framing), so the buffer shrinks once a link settles.
const DECAY_CLEAN_POPS: usize = 200;

/// Headroom above the adaptive target the live depth may reach before the oldest
/// frame is dropped (skip-ahead). Bounds depth to `target + slack`, so a startup
/// burst or a faster-than-us sender clock can't pin latency high — depth tracks
/// the target down instead of riding a ceiling-sized cap.
const DEPTH_SLACK_FRAMES: usize = 2;

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
    /// Current adaptive prebuffer depth (frames), within `[FLOOR_FRAMES, ceiling]`.
    /// Grows on observed jitter, decays toward the floor on a clean link.
    target: usize,
    /// Upper bound on `target` — the user's jitter-slider value. The slider caps
    /// the adaptive depth rather than fixing it, so a quiet link stays low-latency.
    ceiling: usize,
    /// Consecutive clean releases, driving the slow decay back toward the floor.
    clean_pops: usize,
    buf: HashMap<u16, Vec<u8>>,
    /// Sequence to release next; `None` until the first packet sets the origin.
    next_seq: Option<u16>,
    /// Latches true once `target` is first reached, then stays true.
    playing: bool,
    counters: JitterCounters,
}

impl JitterBuffer {
    pub fn new(ceiling_frames: usize) -> JitterBuffer {
        JitterBuffer {
            target: FLOOR_FRAMES,
            ceiling: ceiling_frames.max(FLOOR_FRAMES),
            clean_pops: 0,
            buf: HashMap::new(),
            next_seq: None,
            playing: false,
            counters: JitterCounters::default(),
        }
    }

    /// Current adaptive prebuffer depth in frames — the depth the buffer actually
    /// operates at (floor on a clean link, higher under jitter). The drift servo
    /// targets this, not the ceiling, so its setpoint is reachable.
    pub fn current_target(&self) -> usize {
        self.target
    }

    /// Grow the target toward the ceiling on an observed jitter event.
    fn note_jitter(&mut self) {
        self.target = (self.target + GROW_FRAMES).min(self.ceiling);
        self.clean_pops = 0;
    }

    /// A clean release: after a run of them, decay one frame toward the floor.
    fn note_clean(&mut self) {
        self.clean_pops += 1;
        if self.clean_pops >= DECAY_CLEAN_POPS {
            self.clean_pops = 0;
            if self.target > FLOOR_FRAMES {
                self.target -= 1;
            }
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

    /// Set the adaptive ceiling live (the user moved the jitter slider). The
    /// slider is an upper bound, not a fixed latency: a quiet link sits near the
    /// floor and only climbs toward the ceiling under real jitter.
    pub fn set_target(&mut self, ceiling_frames: usize) {
        self.ceiling = ceiling_frames.max(FLOOR_FRAMES);
        self.target = self.target.min(self.ceiling);
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
                self.note_jitter(); // a late arrival means we under-buffered
                return;
            }
            Some(_) => {}
        }

        self.buf.insert(seq, payload.to_vec());
        self.counters.accepted += 1;

        // Bound the depth to the adaptive target plus headroom, so it tracks the
        // target *down* — a startup burst or a faster-than-us sender clock can't
        // pin latency at a ceiling-sized cap. Drop the oldest frames (closest to
        // the play cursor) and skip the cursor past them.
        let cap = self.target + DEPTH_SLACK_FRAMES;
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
            self.note_clean();

            return JitterOut::Packet(payload);
        }

        // The expected frame is absent. An empty buffer is a genuine underrun:
        // play silence, and drop the play latch so the next fills re-prebuffer to
        // the current (possibly grown) target — rebuilding the cushion only after
        // a real gap, when the audio is already interrupted.
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
            self.note_jitter();

            return JitterOut::Packet(payload);
        }

        self.next_seq = Some(seq.wrapping_add(1));
        self.counters.concealed += 1;
        self.note_jitter(); // a hole means jitter — grow the cushion

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
    fn starts_at_floor_and_releases_in_order() {
        // A clean link starts at the floor (1 frame), not the ceiling, so there's
        // no needless startup latency. The first frame releases immediately.
        let mut jb = JitterBuffer::new(8);
        assert_eq!(jb.current_target(), 1, "begins at the floor, not the ceiling");

        jb.push(0, &[10]);
        assert_eq!(payload(jb.pop()), [10], "one buffered frame is enough to start");

        jb.push(1, &[11]);
        jb.push(2, &[12]);
        assert_eq!(payload(jb.pop()), [11]);
        assert_eq!(payload(jb.pop()), [12]);

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
    fn slider_sets_ceiling_not_fixed_latency() {
        let mut jb = JitterBuffer::new(2);
        assert_eq!(jb.current_target(), 1);

        // Raising the slider only raises the ceiling — a quiet link stays at the
        // floor, so the user doesn't pay latency they aren't using.
        jb.set_target(10);
        assert_eq!(jb.current_target(), 1, "raising the slider adds no latency on a clean link");

        // Grow the target via a hole, then lowering the ceiling clamps it.
        jb.push(1, &[0]);
        jb.push(3, &[0]);
        assert_eq!(payload(jb.pop()), [0]); // seq 1
        assert!(matches!(jb.pop(), JitterOut::Conceal)); // hole at seq 2
        assert_eq!(jb.current_target(), 3);

        jb.set_target(2);
        assert_eq!(jb.current_target(), 2, "lowering the ceiling clamps the target");
    }

    #[test]
    fn grows_toward_ceiling_under_repeated_loss() {
        let mut jb = JitterBuffer::new(8);
        jb.push(0, &[0]);
        assert_eq!(payload(jb.pop()), [0]);

        // Each iteration leaves a one-frame hole, forcing a conceal that grows
        // the cushion. Repeated loss climbs toward the ceiling.
        let mut seq = 2u16;
        for _ in 0..4 {
            jb.push(seq, &[0]);
            assert!(matches!(jb.pop(), JitterOut::Conceal));
            assert_eq!(payload(jb.pop())[0], 0);
            seq += 2;
        }

        assert!(jb.current_target() > 1, "grew under loss");
        assert!(jb.current_target() <= 8, "never past the ceiling");
    }

    #[test]
    fn decays_toward_floor_when_link_is_clean() {
        let mut jb = JitterBuffer::new(8);

        // Grow the target with one hole.
        jb.push(0, &[0]);
        assert_eq!(payload(jb.pop()), [0]);
        jb.push(2, &[0]);
        assert!(matches!(jb.pop(), JitterOut::Conceal));
        assert_eq!(payload(jb.pop())[0], 0);
        let grown = jb.current_target();
        assert!(grown > 1, "target grew");

        // A long run of clean, contiguous frames decays it back to the floor.
        let mut seq = 3u16;
        for _ in 0..(DECAY_CLEAN_POPS * grown) {
            jb.push(seq, &[0]);
            let _ = jb.pop();
            seq = seq.wrapping_add(1);
        }

        assert_eq!(jb.current_target(), 1, "decays to the floor on a clean link");
    }

    #[test]
    fn caps_depth_and_skips_to_live_edge_when_overfilled() {
        // A burst, or a sender clock running faster than our playback, fills the
        // buffer faster than it drains. Depth must stay bounded (no creeping
        // latency over a long call), dropping the oldest and skipping ahead.
        let mut jb = JitterBuffer::new(2); // target starts at floor 1 → cap 3
        for s in 0..100u16 {
            jb.push(s, &[s as u8]);
        }

        assert!(jb.buf.len() <= 3, "depth must stay bounded, got {}", jb.buf.len());

        // Playback resumes at the live edge, not the stale seq 0.
        assert!(payload(jb.pop())[0] >= 96, "skipped the stale backlog to the live edge");
    }

    #[test]
    fn depth_tracks_the_low_target_not_the_ceiling() {
        // Regression: with a high ceiling but a clean link (target at the floor),
        // an accumulated backlog must trim down to `target + slack`, not pin near
        // the ceiling. Pre-fix, the cap was 2×ceiling, so depth rode ~16 frames.
        let mut jb = JitterBuffer::new(8); // generous ceiling
        for s in 0..50u16 {
            jb.push(s, &[0]); // contiguous → no jitter events, target stays at floor
        }

        assert_eq!(jb.current_target(), FLOOR_FRAMES, "clean link keeps target at the floor");
        assert!(
            jb.depth() <= FLOOR_FRAMES + DEPTH_SLACK_FRAMES,
            "depth tracks the low target, not the ceiling: {}",
            jb.depth()
        );
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
