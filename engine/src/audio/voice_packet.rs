//! Native voice wire packet — a self-describing frame parsed by a leading tag.
//!
//! All wire-format knowledge lives here. The tag lets timing instrumentation be
//! added or dropped without a signaling change: a receiver always knows a
//! packet's shape from its first byte, so a stats-on peer and a stats-off peer
//! interoperate. Shrinking the header or removing stats entirely is a change to
//! this module plus the [`stats_enabled`] default — nothing else.
//!
//! Layouts (big-endian fields):
//! - `TAG_PLAIN`: `[tag:u8 | seq:u16 | opus]`
//! - `TAG_TIMED`: `[tag:u8 | seq:u16 | send_ts:u32 | echo_ts:u32 | opus]`
//!
//! `send_ts`/`echo_ts` are milliseconds in the *sender's* own monotonic timebase;
//! the receiver echoes the latest `send_ts` it saw, so round-trip time is
//! `now − echo_ts` with no shared clock (see `native_voice`).

const TAG_PLAIN: u8 = 0x00;
const TAG_TIMED: u8 = 0x01;

/// Header length before the Opus payload, per tag.
const PLAIN_HEADER: usize = 1 + 2;
const TIMED_HEADER: usize = 1 + 2 + 4 + 4;

/// Timestamps carried by a timed packet, both in the sender's monotonic ms clock.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Timing {
    pub send_ts: u32,
    pub echo_ts: u32,
}

/// A parsed inbound packet borrowing its payload from the receive buffer.
pub(crate) struct Parsed<'a> {
    pub seq: u16,
    /// Present only on timed packets (the peer is stamping RTT timestamps).
    pub timing: Option<Timing>,
    pub payload: &'a [u8],
}

/// Whether this peer stamps timing on outbound packets. On by default; set
/// `HEARTH_VOICE_STATS=0` (or `false`) to fall back to the plain wire format.
pub(crate) fn stats_enabled() -> bool {
    match std::env::var("HEARTH_VOICE_STATS") {
        Ok(v) => !matches!(v.trim(), "0" | "false" | "off" | "no"),
        Err(_) => true,
    }
}

/// Encode a voice packet into `out`, returning the written length. `timing`
/// present → timed format (tag 1); absent → plain (tag 0).
pub(crate) fn encode(out: &mut [u8], seq: u16, timing: Option<Timing>, payload: &[u8]) -> usize {
    out[1..3].copy_from_slice(&seq.to_be_bytes());

    let header = match timing {
        None => {
            out[0] = TAG_PLAIN;
            PLAIN_HEADER
        }
        Some(Timing { send_ts, echo_ts }) => {
            out[0] = TAG_TIMED;
            out[3..7].copy_from_slice(&send_ts.to_be_bytes());
            out[7..11].copy_from_slice(&echo_ts.to_be_bytes());
            TIMED_HEADER
        }
    };

    out[header..header + payload.len()].copy_from_slice(payload);

    header + payload.len()
}

/// Parse an inbound datagram. `None` if it's a runt or carries an unknown tag.
pub(crate) fn decode(buf: &[u8]) -> Option<Parsed<'_>> {
    let &tag = buf.first()?;

    match tag {
        TAG_PLAIN if buf.len() >= PLAIN_HEADER => Some(Parsed {
            seq: u16::from_be_bytes([buf[1], buf[2]]),
            timing: None,
            payload: &buf[PLAIN_HEADER..],
        }),
        TAG_TIMED if buf.len() >= TIMED_HEADER => Some(Parsed {
            seq: u16::from_be_bytes([buf[1], buf[2]]),
            timing: Some(Timing {
                send_ts: u32::from_be_bytes([buf[3], buf[4], buf[5], buf[6]]),
                echo_ts: u32::from_be_bytes([buf[7], buf[8], buf[9], buf[10]]),
            }),
            payload: &buf[TIMED_HEADER..],
        }),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_roundtrips() {
        let payload = [1u8, 2, 3, 4];
        let mut buf = [0u8; 64];
        let n = encode(&mut buf, 0x1234, None, &payload);

        let p = decode(&buf[..n]).expect("plain packet parses");
        assert_eq!(p.seq, 0x1234);
        assert!(p.timing.is_none());
        assert_eq!(p.payload, &payload);
    }

    #[test]
    fn timed_roundtrips() {
        let payload = [9u8, 8, 7];
        let timing = Timing { send_ts: 1_000_000, echo_ts: 999_500 };
        let mut buf = [0u8; 64];
        let n = encode(&mut buf, 0xFFFE, Some(timing), &payload);

        let p = decode(&buf[..n]).expect("timed packet parses");
        assert_eq!(p.seq, 0xFFFE);
        assert_eq!(p.timing, Some(timing));
        assert_eq!(p.payload, &payload);
    }

    #[test]
    fn rejects_runt_and_unknown_tag() {
        assert!(decode(&[]).is_none(), "empty");
        assert!(decode(&[TAG_PLAIN, 0x12]).is_none(), "plain header truncated");
        assert!(decode(&[TAG_TIMED, 0, 0, 1, 2, 3]).is_none(), "timed header truncated");
        assert!(decode(&[0x7F, 0, 0, 0, 0]).is_none(), "unknown tag");
    }

    #[test]
    fn timed_packet_is_eight_bytes_larger_than_plain() {
        let mut a = [0u8; 32];
        let mut b = [0u8; 32];
        let plain = encode(&mut a, 1, None, &[0; 4]);
        let timed = encode(&mut b, 1, Some(Timing { send_ts: 0, echo_ts: 0 }), &[0; 4]);

        assert_eq!(timed - plain, 8, "two u32 timestamps");
    }
}
