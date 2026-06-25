//! Echo cancellation tuned for the voice path: speexdsp's linear echo canceller
//! plus residual-echo suppression, but with speex's **own denoise and AGC
//! disabled** — `nnnoiseless` and our envelope AGC already do those, and letting
//! speex double-process thins and pumps the voice. The residual-suppression
//! strength is configurable and adjustable live.
//!
//! This replaces the stock `aec-rs` wrapper, which hardcodes aggressive defaults
//! (≈ −40 dB suppression) and leaves speex's denoise on.

use aec_rs_sys::*;
use std::os::raw::c_void;

/// speexdsp echo canceller + preprocessor. Holds raw C state, so it stays on the
/// capture thread; `Send` lets it move into that thread's closure.
pub struct SpeexAec {
    echo_state: *mut SpeexEchoState,
    preprocess_state: *mut SpeexPreprocessState,
}

unsafe impl Send for SpeexAec {}

impl SpeexAec {
    /// `frame_size` samples per `cancel`, `filter_length` = adaptive-filter tail,
    /// at `sample_rate`. `strength` (0–100) sets the initial residual-echo
    /// suppression aggressiveness.
    pub fn new(frame_size: usize, filter_length: i32, sample_rate: u32, strength: u8) -> Self {
        unsafe {
            let echo_state = speex_echo_state_init(frame_size as i32, filter_length);
            let preprocess_state = speex_preprocess_state_init(frame_size as i32, sample_rate as i32);

            // Link the echo state so the preprocessor performs residual-echo
            // suppression on the linearly-cancelled signal.
            speex_preprocess_ctl(
                preprocess_state,
                SPEEX_PREPROCESS_SET_ECHO_STATE as i32,
                echo_state as *mut c_void,
            );

            let aec = Self { echo_state, preprocess_state };

            // Turn off speex's own denoise + AGC (we run nnnoiseless + our AGC).
            aec.set_ctl(SPEEX_PREPROCESS_SET_DENOISE, 0);
            aec.set_ctl(SPEEX_PREPROCESS_SET_AGC, 0);
            aec.set_strength(strength);

            aec
        }
    }

    /// Pass one `i32` value to a `speex_preprocess_ctl` setter.
    fn set_ctl(&self, request: u32, mut value: i32) {
        unsafe {
            speex_preprocess_ctl(
                self.preprocess_state,
                request as i32,
                &mut value as *mut i32 as *mut c_void,
            );
        }
    }

    /// Residual-echo suppression strength, 0 – 100. The linear echo canceller
    /// always runs; this only scales the *residual* suppressor on top. At 0 the
    /// residual stage is a no-op (0 dB) — linear cancellation only, most natural
    /// voice; at 100 it attenuates idle residual echo hard (−45 dB). The
    /// during-speech attenuation is kept much gentler (max −12 dB) so near-end
    /// voice isn't ducked when you talk over echo (speex's double-talk handling is
    /// weaker than AEC3). More negative = stronger. Live.
    pub fn set_strength(&self, strength: u8) {
        let t = strength.min(100) as f32 / 100.0;
        let suppress = (t * -45.0).round() as i32; // idle: 0 .. -45
        let active = (t * -12.0).round() as i32; // during speech: 0 .. -12

        self.set_ctl(SPEEX_PREPROCESS_SET_ECHO_SUPPRESS, suppress);
        self.set_ctl(SPEEX_PREPROCESS_SET_ECHO_SUPPRESS_ACTIVE, active);
    }

    /// Cancel echo: linear cancellation against the far-end, then residual
    /// suppression. `out` receives the cleaned mic.
    pub fn cancel(&self, mic: &[i16], far: &[i16], out: &mut [i16]) {
        unsafe {
            speex_echo_cancellation(self.echo_state, mic.as_ptr(), far.as_ptr(), out.as_mut_ptr());
            speex_preprocess_run(self.preprocess_state, out.as_mut_ptr());
        }
    }
}

impl Drop for SpeexAec {
    fn drop(&mut self) {
        unsafe {
            speex_echo_state_destroy(self.echo_state);
            speex_preprocess_state_destroy(self.preprocess_state);
        }
    }
}
