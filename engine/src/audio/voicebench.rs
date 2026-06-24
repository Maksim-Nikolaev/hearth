//! Device-independent voice-path microbenchmark.
//!
//! The OBS mouth-to-ear method is dominated by the wireless headset (its latency
//! swings ±15 ms between runs), so it's a poor ruler for *software* tuning. This
//! pushes a synthetic chirp through the **real Opus + UDP transport** the native
//! voice path uses (no mic, no speaker, no headset) and cross-correlates input
//! vs output to report the latency *our code* adds — repeatable to the sample.
//!
//! It isolates the deterministic software cost (Opus encode/decode at 5 ms
//! frames + the UDP hop). The device I/O (~37 ms, measured separately via
//! `engine native`) and the mixer lane (~5–10 ms, device-clock-timed) add on top.
//!
//! Run: `engine voicebench [seconds]` (default 1).

use anyhow::Result;
use audiopus::{coder::Decoder, coder::Encoder, Application, Channels, SampleRate};
use std::net::UdpSocket;
use std::time::Duration;

const SR: usize = 48000;
/// Opus frame in samples — must match `native_voice::FRAME` (5 ms @ 48 kHz).
const FRAME: usize = 240;

/// Run the bench and return the measured (latency_ms, correlation).
pub fn measure(seconds: f64) -> Result<(f64, f32)> {
    let n = (SR as f64 * seconds) as usize;
    let input = chirp(n);

    // Real UDP loopback so the transport (and any framing bug) is exercised.
    let recv = UdpSocket::bind("127.0.0.1:0")?;
    recv.set_read_timeout(Some(Duration::from_millis(200)))?;
    let addr = recv.local_addr()?;
    let send = UdpSocket::bind("127.0.0.1:0")?;

    let enc = Encoder::new(SampleRate::Hz48000, Channels::Mono, Application::LowDelay)?;
    let mut dec = Decoder::new(SampleRate::Hz48000, Channels::Mono)?;
    let mut output: Vec<f32> = Vec::with_capacity(n);
    let mut packet = vec![0u8; 4000];
    let mut rbuf = [0u8; 4000];
    let mut out = vec![0.0f32; FRAME];

    for chunk in input.chunks(FRAME) {
        if chunk.len() < FRAME {
            break;
        }
        let np = enc.encode_float(chunk, &mut packet)?;
        send.send_to(&packet[..np], addr)?;
        let rn = recv.recv(&mut rbuf)?; // loopback delivers immediately
        let nf = dec.decode_float(Some(&rbuf[..rn]), &mut out, false)?;
        output.extend_from_slice(&out[..nf]);
    }

    let max_lag = SR / 20; // search up to 50 ms
    let (lag, score) = xcorr_lag(&input, &output, max_lag);
    Ok((lag as f64 / SR as f64 * 1000.0, score))
}

/// CLI entry: measure and print.
pub fn run(seconds: f64) -> Result<()> {
    let (ms, corr) = measure(seconds)?;
    println!("[voicebench] Opus(5ms lowdelay) + UDP round-trip: {ms:.2} ms  (corr {corr:.3})");
    println!("[voicebench] device I/O ~37 ms + mixer lane ~5-10 ms add on top (see `engine native`)");
    Ok(())
}

/// Linear sine chirp 300→6000 Hz — a sharp cross-correlation peak, no RNG needed.
fn chirp(n: usize) -> Vec<f32> {
    use std::f32::consts::PI;
    let dur = n as f32 / SR as f32;
    (0..n)
        .map(|i| {
            let t = i as f32 / SR as f32;
            let (f0, f1) = (300.0f32, 6000.0f32);
            let phase = 2.0 * PI * (f0 * t + (f1 - f0) * t * t / (2.0 * dur));
            0.3 * phase.sin()
        })
        .collect()
}

/// Lag in [0, max_lag] that best aligns `b` to `a`, with a normalized score.
fn xcorr_lag(a: &[f32], b: &[f32], max_lag: usize) -> (usize, f32) {
    let ea: f64 = a.iter().map(|x| (*x as f64) * (*x as f64)).sum();
    let eb: f64 = b.iter().map(|x| (*x as f64) * (*x as f64)).sum();
    let norm = (ea.sqrt() * eb.sqrt()).max(1e-9);

    let mut best_lag = 0usize;
    let mut best = f64::MIN;
    for lag in 0..max_lag.min(b.len()) {
        let len = a.len().min(b.len() - lag);
        let mut sum = 0.0f64;
        for i in 0..len {
            sum += a[i] as f64 * b[i + lag] as f64;
        }
        if sum > best {
            best = sum;
            best_lag = lag;
        }
    }
    (best_lag, (best / norm) as f32)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Automatable regression guard: the codec+transport round-trip should align
    // cleanly and stay low-latency. Catches codec-config / framing regressions
    // without any audio device.
    #[test]
    fn codec_transport_roundtrip_is_low_latency() {
        let (ms, corr) = measure(0.5).expect("bench ran");
        assert!(corr > 0.7, "input/output should correlate (got {corr})");
        assert!(ms < 20.0, "codec+transport latency should be < 20ms (got {ms})");
    }
}
