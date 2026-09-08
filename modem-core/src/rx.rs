//! Streaming Bell 103 demodulator.
//!
//! The chain is: resample to 8 kHz, correlate against the mark and space
//! tones in quadrature over a one-symbol window, difference the magnitudes
//! to get a soft decision, recover symbol timing with a Gardner detector,
//! slice where the eye is widest, then deframe.
//!
//! The correlator is a boxcar exactly one symbol long, so its output lags
//! the signal by half a symbol and the widest part of the eye lands at the
//! end of each symbol period rather than its centre. The timing loop finds
//! that point on its own; no constant here encodes it.
//!
//! Timing recovery matters more than the correlator. Start-bit detection
//! with fixed mid-bit sampling passes clean loopback and then falls apart
//! once acoustic filtering and callback jitter are involved, so the timing
//! loop tracks continuously instead.
//!
//! # Acquisition is a precondition, not an accident
//!
//! Byte-exact decode from a cold start requires the sender to open with a
//! preamble: **a stretch of alternating symbols, then at least one
//! character time of idle mark, then data.** Both halves are load-bearing
//! and neither substitutes for the other.
//!
//! The alternating part is what the timing loop acquires on. A Gardner
//! detector measures transitions, so idle mark carries no timing
//! information at all and the loop sits wherever it started, however long
//! the idle runs. Measured over all 54 sub-symbol starting offsets: with
//! idle mark alone, 4 to 5 of them slice at the shut part of the eye and
//! mis-decode, and extending the idle from 100 ms to a full second does
//! not fix it.
//!
//! Do not read that as "a longer lead never helps" - it is a lottery, not
//! a monotone. Sweeping the lead one sample at a time, lead lengths 1627
//! to 1653 all score 54 of 54 and the next band scores 49. The winning
//! band is one symbol period wide because the phase a free-running
//! counter arrives at is the lead length modulo the symbol period, and
//! nothing else. A fixture that happens to land in a winning band proves
//! nothing about the next one.
//!
//! The idle mark after it is what puts the deframer back in a known state.
//! 8-N-1 characters sent back to back give it no other way home: a wrong
//! byte alignment that happens to read mark at the stop-bit position is
//! self-consistent and survives indefinitely. Measured: training with no
//! idle after it still loses 4 of 54 offsets to a wrong alignment held for
//! the whole stream. One character time is the figure to quote and to
//! build to; the measured floor is under two symbols, and the margin is
//! not worth spending.
//!
//! One training character is enough, and more does not improve on it.
//!
//! What the preamble guarantees is the **payload**, not the preamble.
//! With both halves in place all 54 offsets deliver the data after the
//! preamble byte-exact with zero framing errors, but the training
//! characters themselves come back altered at 4 of those offsets - 0x55
//! decoding as 0xD5 - because the loop is still pulling in while they go
//! past. **Training must not carry information.** Task 11's overture
//! builds on this: its training stage is there to be locked onto and
//! thrown away, and anything it needs to communicate belongs after the
//! idle mark.
//!
//! Task 11's overture ends in exactly this shape, and Task 9's WAV
//! fixtures and Task 17's half-duplex turnaround both need it.
//!
//! # Carrier gates the slicer, and gating is not locking
//!
//! The correlator's soft decision differences the mark and space
//! magnitudes, so a signal that drives both bands equally is heavily
//! suppressed in it - measured at 62x for mark and space present together,
//! 86x for a tone centred on the answer band - regardless of how strong
//! that signal actually is. This is not a property of the correlator's
//! window length or the 200 Hz mark/space spacing; a full-amplitude tied
//! signal still clears a threshold pinned to a constant, because 1/62 of a
//! loud enough signal is still loud. It only reads as an absence of
//! evidence once the signal is quiet enough that the suppressed residual
//! itself falls under the threshold - see this module's own
//! `two_tones_at_equal_strength_raise_carrier` test, which reproduces the
//! amplitude that does through the real correlator. Left ungated, that
//! suppression is exactly what lets the slicer fabricate bytes and framing
//! errors from a dead line's noise. [`carrier::CarrierDetector`] watches
//! the *sum* of the two magnitudes instead, and `symbol_boundary` only
//! pushes a bit into the deframer while it reports carrier present.
//!
//! Carrier present is not the same claim as "the timing loop is locked".
//! A Gardner detector measures transitions, and idle mark alone raises
//! carrier well before - or entirely without - the loop ever seeing one.
//! Do not read `carrier_detected()` as a proxy for acquisition.
//!
//! Nothing here allocates per block. The ring buffers and the scratch are
//! sized at construction, and the event queue is preallocated to its cap.

use alloc::collections::VecDeque;
use alloc::vec;
use alloc::vec::Vec;

use libm::{cos, hypot, round, sin};

use crate::carrier::{CarrierDetector, Event};
use crate::frame::Deframer;
use crate::resample::Resampler;
use crate::{samples_per_symbol, tones, Config, DSP_RATE};

/// Timing loop gain. Measured, not guessed: this is a proportional-only
/// loop, so it holds a static phase offset proportional to the clock error
/// it is absorbing, and the gain sets how much clock error it can absorb
/// before that offset walks the slicer out of the eye.
///
/// At 0.15 a 2100-byte payload decodes byte-exact through +/-2.5% of
/// sample-clock offset and fails at +/-3%. Below 0.1 the pull-in range
/// drops under the 2% the tests assert. Noise is not what constrains this:
/// byte error rate is flat from gain 0.05 to 0.2 all the way down to 6 dB
/// SNR, where the matched filter's own processing gain is what is doing
/// the work.
const GARDNER_GAIN: f64 = 0.15;

/// Bound on the carrier event queue. Was two unrelated literal `64`s (the
/// preallocation and the drop-oldest check) that could silently drift
/// apart; one constant makes that impossible.
const EVENT_QUEUE_CAP: usize = 64;

pub struct Rx {
    // Quadrature correlator state. One symbol of product history per tone,
    // held as a ring so the sliding sum costs nothing to maintain.
    win: usize,
    pos: usize,
    filled: bool,
    mark_i: Vec<f64>,
    mark_q: Vec<f64>,
    space_i: Vec<f64>,
    space_q: Vec<f64>,
    phase_m: f64,
    phase_s: f64,
    inc_m: f64,
    inc_s: f64,

    // Symbol timing.
    sym_phase: f64,
    sym_inc: f64,
    last_sym: f64,
    mid_sym: f64,
    have_mid: bool,

    deframer: Deframer,
    out: Vec<u8>,
    resampler: Resampler,
    /// Device-rate input converted to f64, ready for the resampler. Reused,
    /// resized only on growth.
    in64: Vec<f64>,
    /// Resampler output at DSP_RATE. Reused, resized only on growth.
    scratch: Vec<f64>,

    carrier: CarrierDetector,
    events: VecDeque<Event>,
}

impl Rx {
    pub fn new(cfg: Config) -> Self {
        assert!(cfg.sample_rate > 0, "sample_rate must be greater than zero");
        let (mark, space) = tones(cfg.role);
        let win = round(samples_per_symbol()) as usize;
        Self {
            win,
            pos: 0,
            filled: false,
            mark_i: vec![0.0; win],
            mark_q: vec![0.0; win],
            space_i: vec![0.0; win],
            space_q: vec![0.0; win],
            phase_m: 0.0,
            phase_s: 0.0,
            inc_m: core::f64::consts::TAU * mark / DSP_RATE,
            inc_s: core::f64::consts::TAU * space / DSP_RATE,
            sym_phase: 0.0,
            sym_inc: 1.0 / samples_per_symbol(),
            last_sym: 0.0,
            mid_sym: 0.0,
            have_mid: false,
            deframer: Deframer::new(),
            out: Vec::new(),
            resampler: Resampler::new(cfg.sample_rate as f64, DSP_RATE),
            in64: Vec::new(),
            scratch: Vec::new(),

            carrier: CarrierDetector::new(),
            // Capacity matches the cap enforced in push_sample, so the
            // bounded queue never reallocates once running: the only
            // allocation is this one, at construction.
            events: VecDeque::with_capacity(EVENT_QUEUE_CAP),
        }
    }

    pub fn framing_errors(&self) -> usize {
        self.deframer.framing_errors()
    }

    /// Hands the receiver the next contiguous block of samples. Blocks must
    /// be in order and gapless; a discontinuity costs symbol lock, and now
    /// also the resampler's own fractional phase and filter history.
    pub fn write(&mut self, input: &[f32]) {
        if self.in64.len() < input.len() {
            self.in64.resize(input.len(), 0.0);
        }
        for (o, &i) in self.in64.iter_mut().zip(input) {
            *o = i as f64;
        }

        let need = self.resampler.max_output_len(input.len());
        if self.scratch.len() < need {
            self.scratch.resize(need, 0.0);
        }
        let n = self
            .resampler
            .process(&self.in64[..input.len()], &mut self.scratch[..need]);
        for i in 0..n {
            self.push_sample(self.scratch[i]);
        }
    }

    fn push_sample(&mut self, s: f64) {
        // Writing at pos overwrites the oldest product, so summing the ring
        // gives the correlation over the last symbol. That sum is the
        // matched filter for FSK.
        self.mark_i[self.pos] = s * cos(self.phase_m);
        self.mark_q[self.pos] = s * sin(self.phase_m);
        self.space_i[self.pos] = s * cos(self.phase_s);
        self.space_q[self.pos] = s * sin(self.phase_s);

        self.phase_m += self.inc_m;
        if self.phase_m >= core::f64::consts::TAU {
            self.phase_m -= core::f64::consts::TAU;
        }
        self.phase_s += self.inc_s;
        if self.phase_s >= core::f64::consts::TAU {
            self.phase_s -= core::f64::consts::TAU;
        }

        self.pos += 1;
        if self.pos >= self.win {
            self.pos = 0;
            self.filled = true;
        }
        if !self.filled {
            return;
        }

        let (soft, energy) = self.correlate();
        if let Some(ev) = self.carrier.update(energy) {
            if self.events.len() >= EVENT_QUEUE_CAP {
                self.events.pop_front();
            }
            self.events.push_back(ev);
        }

        let prev = self.sym_phase;
        self.sym_phase += self.sym_inc;
        if prev < 0.5 && self.sym_phase >= 0.5 {
            self.mid_sym = soft;
            self.have_mid = true;
        }
        if self.sym_phase >= 1.0 {
            self.sym_phase -= 1.0;
            self.symbol_boundary(soft);
        }
    }

    /// Returns (soft decision, total energy). The soft decision is mark
    /// magnitude minus space magnitude - positive means mark. The energy is
    /// their sum, which the carrier detector needs and which the difference
    /// throws away.
    fn correlate(&self) -> (f64, f64) {
        let mut mi = 0.0;
        let mut mq = 0.0;
        let mut si = 0.0;
        let mut sq = 0.0;
        for i in 0..self.win {
            mi += self.mark_i[i];
            mq += self.mark_q[i];
            si += self.space_i[i];
            sq += self.space_q[i];
        }
        let m = hypot(mi, mq);
        let s = hypot(si, sq);
        (m - s, (m + s) / self.win as f64)
    }

    fn symbol_boundary(&mut self, soft: f64) {
        // Gardner timing error detector: e = (current - previous) * midpoint.
        // It needs no knowledge of the data and works on the soft decision,
        // which is what lets it track continuously rather than re-acquiring
        // from every start bit.
        if self.have_mid {
            // Normalise so loop gain does not scale with signal level. The
            // error is a product of two amplitude terms - a difference of
            // decisions and a midpoint - so it needs dividing by the level
            // twice. Dividing once leaves the effective gain riding on the
            // input amplitude, which at these signal levels is a factor of
            // eight and turns a stable loop into an unstable one.
            let mag = soft.abs() + self.last_sym.abs() + 1e-9;
            let e = (soft - self.last_sym) * self.mid_sym / (mag * mag);

            // Adding advances the clock. Sampling late puts the midpoint
            // past the transition and into the new symbol, which makes e
            // positive, so late must pull the next boundary forward.
            // Subtracting locks the slicer onto the transitions instead of
            // the symbols, which is the one place the eye is shut.
            self.sym_phase += GARDNER_GAIN * e;

            // Deliberately not wrapped. sym_phase here is the small residue
            // left after the boundary subtracted 1.0, so any retard takes
            // it negative and that is exactly right: the accumulator simply
            // takes a few more samples to reach 1.0 again. Wrapping a
            // negative residue up to ~0.99 fires a second boundary on the
            // very next sample, which is a whole inserted symbol. That one
            // line is the difference between a receiver that tracks and one
            // that only works when the gain is zero.
        }
        self.last_sym = soft;
        self.have_mid = false;

        // Mark on a tie. soft is mark energy minus space energy, so a tie
        // is an absence of evidence, and the idle line state is mark.
        // Slicing ties as space turns digital silence into an endless run
        // of start bits and one framing error every ten symbols.
        //
        // Gated on carrier: without a signal above the noise floor, this
        // slicer has nothing to slice, and pushing its tie-break into the
        // deframer regardless is exactly what turns line noise into
        // fabricated bytes and framing errors.
        if self.carrier.detected() {
            if let Some(b) = self.deframer.push_bit(soft >= 0.0) {
                self.out.push(b);
            }
        }
    }

    /// Drains demodulated bytes. Returns how many were written.
    pub fn read(&mut self, buf: &mut [u8]) -> usize {
        let n = self.out.len().min(buf.len());
        buf[..n].copy_from_slice(&self.out[..n]);
        self.out.drain(..n);
        n
    }

    /// Whether in-band energy is currently above the noise floor by enough
    /// to call it carrier. Says nothing about whether the timing loop is
    /// locked - a Gardner detector cannot acquire on a constant tone, so
    /// idle mark alone raises this while the loop stays free-running.
    pub fn carrier_detected(&self) -> bool {
        self.carrier.detected()
    }

    /// Drains one carrier transition event. Returns None when the queue is
    /// empty.
    pub fn events(&mut self) -> Option<Event> {
        self.events.pop_front()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tx::Tx;
    use crate::{Duplex, Role};
    use alloc::vec::Vec;

    fn cfg(role: Role, rate: u32) -> Config {
        Config {
            sample_rate: rate,
            role,
            duplex: Duplex::HalfPingPong,
        }
    }

    /// Modulates payload, demodulates it, returns what came back.
    ///
    /// The lead-in is real: 100 ms of idle mark is read out of `tx` and
    /// pushed through `rx` *before* the payload is queued, so the
    /// correlator window is full and the deframer is idle when the first
    /// start bit arrives. It does not acquire symbol timing - idle mark has
    /// no transitions to acquire on - which is why this fixture starts the
    /// receiver in step with the transmitter and
    /// `receiver_joining_mid_symbol_recovers_byte_alignment` carries the
    /// arbitrary-offset case with the full preamble.
    ///
    /// Block size is deliberately awkward - 733 samples is never a whole
    /// number of symbols at any rate here, so nothing can accidentally
    /// depend on a block boundary landing on a symbol boundary.
    fn loopback(payload: &[u8], role: Role, rate: u32, block: usize) -> Vec<u8> {
        let c = cfg(role, rate);
        let mut tx = Tx::new(c);
        let mut rx = Rx::new(c);

        let mut out = Vec::new();
        let mut buf = vec![0.0f32; block];
        let mut got = [0u8; 256];

        let mut sent = 0;
        while sent < rate as usize / 10 {
            tx.read(&mut buf);
            rx.write(&buf);
            rx.read(&mut got);
            sent += block;
        }

        tx.write(payload);
        let total = (payload.len() * 10) * rate as usize / 300 + rate as usize;
        let mut sent = 0;
        while sent < total {
            tx.read(&mut buf);
            rx.write(&buf);
            let n = rx.read(&mut got);
            out.extend_from_slice(&got[..n]);
            sent += block;
        }
        out
    }

    /// Deterministic low-level noise at a chosen amplitude, no carrier
    /// anywhere. Shared by every noise-rejection test in this file so they
    /// all draw from the same generator.
    fn noise_buf(amplitude: f32, n: usize, seed: u64) -> Vec<f32> {
        let mut state = seed;
        (0..n)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                ((state >> 40) as f32 / 8_388_608.0 - 1.0) * amplitude
            })
            .collect()
    }

    /// Byte-exact comparison with a failure message you can read.
    /// `assert_eq!` on a 2100-byte payload prints both vectors in full and
    /// buries the one fact that matters, which is where they first parted.
    fn assert_same(got: &[u8], want: &[u8], ctx: &str) {
        if got == want {
            return;
        }
        let at = got.iter().zip(want).position(|(a, b)| a != b);
        panic!(
            "{ctx}: recovered {} of {} bytes, first difference at {at:?}, head {:?}",
            got.len(),
            want.len(),
            core::str::from_utf8(&got[..got.len().min(48)])
        );
    }

    #[test]
    fn loopback_originate() {
        let payload = b"CONNECT 300";
        let got = loopback(payload, Role::Originate, 8000, 733);
        assert_same(&got, payload, "originate");
    }

    #[test]
    fn loopback_answer() {
        let payload = b"NO CARRIER";
        let got = loopback(payload, Role::Answer, 8000, 733);
        assert_same(&got, payload, "answer");
    }

    /// All 256 values. A reversed bit order or a swapped mark and space
    /// produces plausible-looking output on ASCII alone.
    #[test]
    fn loopback_all_byte_values() {
        let payload: Vec<u8> = (0..=255u8).collect();
        let got = loopback(&payload, Role::Originate, 8000, 733);
        assert_same(&got, &payload, "all byte values");
    }

    /// 20000 bits. Any symbol clock error compounds here: rounding
    /// samples-per-symbol to 27 drifts a whole symbol every forty
    /// characters, so this fails long before the end.
    #[test]
    fn loopback_long_payload_does_not_drift() {
        let mut payload = Vec::new();
        for _ in 0..100 {
            payload.extend_from_slice(b"The quick brown fox. ");
        }
        let got = loopback(&payload, Role::Originate, 8000, 733);
        assert_same(&got, &payload, "drifted");
    }

    /// Two sound cards never agree on the sample rate, and the timing loop
    /// is the only thing that covers the difference. Here the transmitter
    /// believes the device runs at 8160 or 7840 Hz while the receiver
    /// believes it runs at 8000, which is a 2% clock offset between the two
    /// ends of a real link.
    ///
    /// This is the test that makes the Gardner block load-bearing. Every
    /// other loopback in this file runs both ends off one clock, where a
    /// free-running symbol counter is already exact, so all of them pass
    /// with the timing correction deleted outright.
    ///
    /// It is also the only place the timing loop's scale invariance is
    /// visible, which is why the input level is swept over a factor of a
    /// hundred. `e` is a product of two signal-amplitude terms, so the
    /// level has to divide out of it twice; dividing once leaves the
    /// effective loop gain riding on the input level, and the microphone
    /// and the room set that, not this code. A single-level test cannot
    /// see the difference - `e /= mag` with a correspondingly smaller
    /// GARDNER_GAIN decodes this payload perfectly at amplitude 1.0 and
    /// loses a third of it at 0.1.
    ///
    /// Task 7 added an alternating training preamble after the idle-mark
    /// settle period, which earlier revisions of this test did not need.
    /// The placeholder resampler this crate used to have was near enough
    /// to zero-delay that the free-running symbol counter's phase at the
    /// first real transition happened, by luck, to land in a winning band
    /// for both tx_rate cases. The real resampler's low-pass has a genuine
    /// group delay - half the filter, not "a few taps" as an earlier draft
    /// of this comment claimed: (N - 1) / 2 samples for an N-tap linear
    /// phase FIR, which is 31 taps at the 8 kHz reference rate and 186
    /// taps at 48 kHz. At tx_rate 8160 Hz (64 taps there) that is 31.5
    /// samples, 3.86 ms, 1.16 symbol periods at 300 baud - "comparable to
    /// a symbol period" was the right description, the tap count was not.
    /// Idle mark carries no timing information for the loop to acquire
    /// against (see this module's own doc), so the shifted phase landed
    /// tx_rate 8160 in a losing band: the payload's first byte decoded as
    /// 0x50 instead of 0x54, one bit wrong, with everything after it -
    /// once the loop had a real transition to pull in on - correct.
    /// Adding the same acquisition preamble this file already establishes
    /// elsewhere as required (see `receiver_joining_mid_symbol_recovers_
    /// byte_alignment`) fixed it at every amplitude and both tx_rates;
    /// this is that fix, not a loosened assertion - the content
    /// comparison below still demands the payload byte-exact.
    #[test]
    fn loopback_tracks_a_two_percent_sample_clock_offset() {
        let mut payload = Vec::new();
        for _ in 0..100 {
            payload.extend_from_slice(b"The quick brown fox. ");
        }
        for amplitude in [1.0f32, 0.1, 10.0] {
            for tx_rate in [8160u32, 7840] {
                let mut tx = Tx::new(cfg(Role::Originate, tx_rate));
                let mut rx = Rx::new(cfg(Role::Originate, 8000));

                let mut out = Vec::new();
                let mut buf = vec![0.0f32; 733];
                let mut got = [0u8; 256];

                let mut sent = 0;
                while sent < tx_rate as usize / 10 {
                    tx.read(&mut buf);
                    for v in buf.iter_mut() {
                        *v *= amplitude;
                    }
                    rx.write(&buf);
                    rx.read(&mut got);
                    sent += 733;
                }

                // Alternating preamble the Gardner loop can actually
                // acquire on, then one character time of idle mark to put
                // the deframer back in a known state - see this module's
                // doc for why both halves are load-bearing.
                tx.write(&[0x55, 0x55]);
                let acquire = 2 * 10 * tx_rate as usize / 300 + tx_rate as usize / 30;
                let mut sent = 0;
                while sent < acquire {
                    tx.read(&mut buf);
                    for v in buf.iter_mut() {
                        *v *= amplitude;
                    }
                    rx.write(&buf);
                    rx.read(&mut got); // discard the training bytes
                    sent += 733;
                }

                tx.write(&payload);
                let total = tx_rate as usize * 2 + (payload.len() * 10) * tx_rate as usize / 300;
                let mut sent = 0;
                while sent < total {
                    tx.read(&mut buf);
                    for v in buf.iter_mut() {
                        *v *= amplitude;
                    }
                    rx.write(&buf);
                    let n = rx.read(&mut got);
                    out.extend_from_slice(&got[..n]);
                    sent += 733;
                }
                let ctx = alloc::format!("tx rate {tx_rate}, amplitude {amplitude}");
                // Content, not a byte count. A slipped symbol that happens
                // to stay frame-aligned yields exactly 2100 bytes and zero
                // framing errors while every one of them is wrong, so
                // neither aggregate can stand in for comparing what
                // actually came back.
                assert_same(&out, &payload, &ctx);
                assert_eq!(rx.framing_errors(), 0, "{ctx} produced framing errors");
            }
        }
    }

    /// The receiver does not get to choose where in a symbol it starts. A
    /// WAV file opened at an arbitrary sample, a cpal stream that begins
    /// when the device is ready, and a half-duplex turnaround all hand it a
    /// sub-symbol offset it had no say in. Every one of them must recover
    /// byte alignment.
    ///
    /// The preamble here is the one the module doc states as a
    /// precondition, and both halves of it are load-bearing. Measured over
    /// these 54 offsets at this lead length: drop the training characters
    /// and 4 offsets slice at the shut part of the eye and mis-decode,
    /// because a Gardner detector cannot acquire on a tone that never
    /// changes. Drop the idle mark after the training and 4 offsets latch a
    /// wrong byte alignment while the loop is still pulling in and hold it
    /// for the whole stream, because back-to-back 8-N-1 characters give the
    /// deframer no way home once a wrong alignment reads mark at the
    /// stop-bit position.
    ///
    /// 54 offsets is two whole symbols, so the sweep cannot sit on one
    /// phase of the symbol clock and call it coverage.
    #[test]
    fn receiver_joining_mid_symbol_recovers_byte_alignment() {
        let c = cfg(Role::Originate, 8000);
        let payload = b"The quick brown fox jumps over the lazy dog. 0123456789";

        fn emit(tx: &mut Tx, air: &mut Vec<f32>, n: usize) {
            let mut b = vec![0.0f32; n];
            tx.read(&mut b);
            air.extend_from_slice(&b);
        }

        let mut tx = Tx::new(c);
        let mut air = Vec::new();
        emit(&mut tx, &mut air, 1600); // quiet line, correlator window fills
        tx.write(&[0x55, 0x55]); // training: alternating symbols to lock to
        emit(&mut tx, &mut air, 2 * 10 * 8000 / 300);
        emit(&mut tx, &mut air, 267); // one character time of idle mark
        tx.write(payload);
        emit(&mut tx, &mut air, payload.len() * 10 * 8000 / 300 + 8000);

        for skip in 0..54 {
            let mut rx = Rx::new(c);
            let mut out = Vec::new();
            let mut got = [0u8; 256];
            let mut i = skip;
            while i + 733 <= air.len() {
                rx.write(&air[i..i + 733]);
                let n = rx.read(&mut got);
                out.extend_from_slice(&got[..n]);
                i += 733;
            }
            // ends_with, not equality: the two training characters are part
            // of the stream and legitimately come back too, and at 4 of
            // these 54 offsets the second one arrives as 0xD5 rather than
            // 0x55 because the loop is still pulling in while it goes past.
            // What must hold is that everything after them is the payload
            // exactly, with nothing lost, inserted or shifted.
            assert!(
                out.ends_with(payload),
                "joined {skip} samples in: {} bytes, {} framing errors, got {:?}",
                out.len(),
                rx.framing_errors(),
                core::str::from_utf8(&out)
            );
            // ends_with on its own constrains nothing ahead of the payload,
            // and a zero error count does not bound it either: a deframer
            // that opens mid-frame delivers a spurious byte with no framing
            // error at all and still satisfies the assertion above. The
            // length is invariant where equality is not, so pin it.
            assert_eq!(
                out.len(),
                2 + payload.len(),
                "joined {skip} samples in and delivered bytes outside the two training characters"
            );
            assert_eq!(
                rx.framing_errors(),
                0,
                "joined {skip} samples in and produced framing errors"
            );
        }
    }

    /// Every audio device opens on silence and carries later, so this is
    /// the ordinary start-up path, not an edge case.
    ///
    /// Digital silence carries no evidence of anything, so the receiver
    /// must sit in the idle line state and emit nothing at all. Slicing a
    /// tie as space instead reads silence as a start bit and manufactures a
    /// framing error every ten symbols - thirty a second of invented link
    /// damage, on a line with nothing on it.
    ///
    /// Then the carrier comes up on the same receiver, and that is the half
    /// the first two assertions cannot cover. On exact silence both tone
    /// magnitudes are zero, so the timing loop's normalising denominator is
    /// zero too; without the epsilon guarding it the error goes NaN,
    /// sym_phase goes NaN, every comparison against it is false and the
    /// receiver never decodes anything again. A receiver bricked that way
    /// is indistinguishable from a healthy idle one by byte count and error
    /// count alike - both are zero, which is exactly what the assertions
    /// above demand. It only surfaces when something finally arrives.
    #[test]
    fn silence_is_read_as_idle_mark() {
        let c = cfg(Role::Originate, 8000);
        let mut rx = Rx::new(c);
        let quiet = vec![0.0f32; 800];
        let mut got = [0u8; 256];
        let mut bytes = 0;
        for _ in 0..50 {
            rx.write(&quiet);
            bytes += rx.read(&mut got);
        }
        assert_eq!(bytes, 0, "silence produced bytes");
        assert_eq!(
            rx.framing_errors(),
            0,
            "silence produced framing errors, so the slicer is reading no signal as space"
        );

        let payload = b"CONNECT 300";
        let mut tx = Tx::new(c);
        let mut buf = vec![0.0f32; 733];
        let mut sent = 0;
        while sent < 800 {
            tx.read(&mut buf);
            rx.write(&buf);
            rx.read(&mut got);
            sent += 733;
        }
        tx.write(payload);
        let mut out = Vec::new();
        let mut sent = 0;
        while sent < payload.len() * 10 * 27 + 8000 {
            tx.read(&mut buf);
            rx.write(&buf);
            let n = rx.read(&mut got);
            out.extend_from_slice(&got[..n]);
            sent += 733;
        }
        assert_same(&out, payload, "carrier after silence");
    }

    /// read must consume what it hands back. Returning the same bytes again
    /// on the next call still satisfies every `contains` check in this
    /// file, because the payload is in there, just repeatedly, so nothing
    /// else here would notice the queue never draining.
    #[test]
    fn read_drains_what_it_returns() {
        let c = cfg(Role::Originate, 8000);
        let mut tx = Tx::new(c);
        let mut rx = Rx::new(c);
        tx.write(b"AB");

        let mut buf = vec![0.0f32; 733];
        let mut got = [0u8; 256];
        let mut out = Vec::new();
        for _ in 0..8 {
            tx.read(&mut buf);
            rx.write(&buf);
            let n = rx.read(&mut got);
            out.extend_from_slice(&got[..n]);
        }
        assert_eq!(out, b"AB", "read handed the same bytes back more than once");
        assert_eq!(rx.read(&mut got), 0, "queue still holds drained bytes");
    }

    #[test]
    fn loopback_at_48khz() {
        let payload = b"CONNECT 300 at 48 kHz";
        let got = loopback(payload, Role::Originate, 48000, 4096);
        assert_same(&got, payload, "48 kHz");
    }

    /// The receiver must not allocate per block once running.
    #[test]
    fn write_does_not_allocate_after_the_first_call() {
        let c = cfg(Role::Originate, 8000);
        let mut rx = Rx::new(c);
        let buf = vec![0.0f32; 512];
        rx.write(&buf);
        let ptr = rx.scratch.as_ptr();
        for i in 0..50 {
            rx.write(&buf);
            assert_eq!(rx.scratch.as_ptr(), ptr, "scratch reallocated, call {i}");
        }
    }

    /// Round 1 review finding: the test above runs at 8000 Hz, the
    /// resampler's identity bypass, so it cannot see whether `in64` or
    /// `scratch` allocate in the hot path of a real (non-identity)
    /// resampling call. This drives the same check through an actual
    /// 48 kHz decimation.
    #[test]
    fn write_does_not_allocate_after_the_first_call_at_48khz() {
        let c = cfg(Role::Originate, 48000);
        let mut rx = Rx::new(c);
        let buf = vec![0.0f32; 512];
        rx.write(&buf);
        let ptr = rx.scratch.as_ptr();
        for i in 0..50 {
            rx.write(&buf);
            assert_eq!(
                rx.scratch.as_ptr(),
                ptr,
                "scratch reallocated at 48 kHz, call {i}"
            );
        }
    }

    /// A clean loopback must produce no framing errors at all. Without this
    /// the suite would accept a receiver that recovered the payload while
    /// silently mangling frames around it.
    #[test]
    fn clean_loopback_has_no_framing_errors() {
        let c = cfg(Role::Originate, 8000);
        let mut tx = Tx::new(c);
        let mut rx = Rx::new(c);
        tx.write(b"The quick brown fox jumps over the lazy dog");

        let mut buf = vec![0.0f32; 733];
        let mut got = [0u8; 256];
        for _ in 0..40 {
            tx.read(&mut buf);
            rx.write(&buf);
            rx.read(&mut got);
        }
        assert_eq!(
            rx.framing_errors(),
            0,
            "clean loopback produced framing errors"
        );
    }

    #[test]
    fn carrier_rises_on_signal_and_falls_on_silence() {
        let c = cfg(Role::Originate, 8000);
        let mut tx = Tx::new(c);
        let mut rx = Rx::new(c);

        let mut buf = vec![0.0f32; 8000];
        tx.read(&mut buf);
        rx.write(&buf);
        assert!(
            rx.carrier_detected(),
            "no carrier on a strong idle mark tone"
        );

        rx.write(&vec![0.0f32; 8000]);
        assert!(
            !rx.carrier_detected(),
            "carrier held through a second of silence"
        );
    }

    /// A brief pause in traffic must not report NO CARRIER. Real modems
    /// hold through gaps and so must this.
    #[test]
    fn carrier_holds_through_a_short_gap() {
        let c = cfg(Role::Originate, 8000);
        let mut tx = Tx::new(c);
        let mut rx = Rx::new(c);

        let mut buf = vec![0.0f32; 8000];
        tx.read(&mut buf);
        rx.write(&buf);
        assert!(rx.carrier_detected());

        rx.write(&vec![0.0f32; 800]); // 100 ms, well inside the hold-off
        assert!(rx.carrier_detected(), "carrier dropped during a 100 ms gap");
    }

    #[test]
    fn carrier_emits_events_in_order() {
        let c = cfg(Role::Originate, 8000);
        let mut tx = Tx::new(c);
        let mut rx = Rx::new(c);

        let mut buf = vec![0.0f32; 8000];
        tx.read(&mut buf);
        rx.write(&buf);
        assert_eq!(rx.events(), Some(Event::CarrierUp));
        assert_eq!(
            rx.events(),
            None,
            "queued more than one event on a single rise"
        );

        rx.write(&vec![0.0f32; 8000]);
        assert_eq!(rx.events(), Some(Event::CarrierDown));
        assert_eq!(rx.events(), None);
    }

    /// Carrier being up must not be read as the timing loop being locked.
    /// A Gardner detector cannot acquire on a constant tone, so idle mark
    /// raises carrier while the loop is still free-running.
    #[test]
    fn carrier_up_does_not_imply_symbol_lock() {
        let c = cfg(Role::Originate, 8000);
        let mut tx = Tx::new(c);
        let mut rx = Rx::new(c);
        let mut buf = vec![0.0f32; 8000];
        tx.read(&mut buf); // idle mark only, no transitions
        rx.write(&buf);
        assert!(rx.carrier_detected());
        let mut got = [0u8; 256];
        assert_eq!(rx.read(&mut got), 0, "idle mark alone produced bytes");
    }

    /// A count is not enough here: a bounded queue that drops the *newest*
    /// event on overflow, or that drops from the middle, or that simply
    /// never grows past the cap by silently discarding the arrival
    /// instead, all leave the same drained length as the correct
    /// drop-the-oldest behaviour, so a length assertion alone would pass
    /// under any of them. This drives 65 events through a 64-slot queue
    /// without draining it and checks which specific event survived at
    /// each end.
    ///
    /// 33 rises and 32 completed drops strictly alternate CarrierUp and
    /// CarrierDown starting from Up, so the correct result - the oldest
    /// event, the very first CarrierUp, evicted - leaves a drained
    /// sequence that starts on a CarrierDown and ends on a CarrierUp.
    /// That is a real signature: a trace beginning on Down is exactly
    /// what "the earliest Up fell off the front" looks like, and no
    /// wrong eviction policy of the same final length reproduces it.
    #[test]
    fn events_queue_drops_the_oldest_not_the_newest_when_full() {
        let c = cfg(Role::Originate, 8000);
        let mut tx = Tx::new(c);
        let mut rx = Rx::new(c);

        let mut carrier_buf = vec![0.0f32; 800]; // 100 ms, ample to rise
        let silence_buf = vec![0.0f32; 4800]; // 600 ms, clears the 500 ms hold-off

        // 33 rises, 32 completed drops: 65 events into a 64-slot queue,
        // never drained in between.
        for cycle in 0..33 {
            tx.read(&mut carrier_buf);
            rx.write(&carrier_buf);
            if cycle < 32 {
                rx.write(&silence_buf);
            }
        }

        let mut drained = Vec::new();
        while let Some(ev) = rx.events() {
            drained.push(ev);
        }

        assert_eq!(drained.len(), 64, "queue did not cap at 64 events");
        assert_eq!(
            drained.first(),
            Some(&Event::CarrierDown),
            "the oldest event - the first CarrierUp - was not the one evicted"
        );
        assert_eq!(
            drained.last(),
            Some(&Event::CarrierUp),
            "the newest event was evicted instead of the oldest"
        );
    }

    /// Task 5's central finding, proven through the real wiring rather than
    /// asserted. Fed the difference (`soft.abs()`, with or without the
    /// `/win` normalisation `energy` applies), two equal-strength tones
    /// read as heavily suppressed rather than absent - the earlier attempt
    /// at this test used full amplitude, where even a suppressed residual
    /// still clears a threshold pinned to a constant, and wrongly
    /// concluded the failure was unreachable. It is reachable; it just
    /// needs a quiet enough signal. Measured through this correlator: two
    /// 0.006-amplitude tones, mark and space together, hold a sum-based
    /// energy level of about 6e-3 (ratio 6 against `INITIAL_FLOOR`,
    /// clearing `RISE_RATIO`) but a difference-based level under 3e-4
    /// either way `/win` is or is not applied - below `INITIAL_FLOOR`
    /// itself, so neither difference-fed variant ever asserts carrier.
    #[test]
    fn two_tones_at_equal_strength_raise_carrier() {
        let c = cfg(Role::Originate, 8000);
        let mut rx = Rx::new(c);
        let (mark, space) = tones(Role::Originate);
        let buf: Vec<f32> = (0..16_000)
            .map(|i| {
                let t = i as f64 / DSP_RATE;
                (0.006 * sin(core::f64::consts::TAU * mark * t)
                    + 0.006 * sin(core::f64::consts::TAU * space * t)) as f32
            })
            .collect();
        rx.write(&buf);
        assert!(
            rx.carrier_detected(),
            "two tones at equal strength must raise carrier"
        );
    }

    /// The reason this task exists, measured across a range rather than at
    /// one point, from a cold start (no prior idle time - see
    /// `noise_rejection_at_reference_amplitude_holds_across_idle_
    /// durations` for the same property across idle durations). A single
    /// amplitude cannot show where the detector's noise-rejection ceiling
    /// actually sits, and Task 6's fix round 1 shipped with that ceiling
    /// far too low - genuine microphone self-noise (about -38 dBFS,
    /// amplitude 0.0126) falsely asserted carrier and, because the floor
    /// could never move once detected, held that lock for as long as the
    /// noise itself continued. Swapping to a floor that freezes on
    /// detection instead of merely decaying slowly moved the measured
    /// cold-start ceiling from about 0.012 to precisely between 0.0205
    /// (passes) and 0.021 (fails) - documented as a measured pair of
    /// numbers in `carrier.rs`'s `INITIAL_FLOOR` comment, not the "0.02 to
    /// 0.03" range an earlier report gave from testing only the endpoints.
    #[test]
    fn noise_on_a_dead_line_produces_no_bytes() {
        for amplitude in [1e-4f32, 1e-3, 5e-3, 1e-2, 0.0126, 0.02] {
            let c = cfg(Role::Originate, 8000);
            let mut rx = Rx::new(c);

            // Deterministic noise, no carrier anywhere, scaled per sweep
            // point.
            let noise = noise_buf(amplitude, 40_000, 0x2545F491_4F6CDD1D);

            let mut got = [0u8; 256];
            let mut total = 0;
            for chunk in noise.chunks(733) {
                rx.write(chunk);
                total += rx.read(&mut got);
            }
            assert_eq!(
                total, 0,
                "amplitude {amplitude}: fabricated {total} bytes from noise on a dead line"
            );
            assert_eq!(
                rx.framing_errors(),
                0,
                "amplitude {amplitude}: fabricated framing errors from noise"
            );
        }
    }

    /// This is the **cold-start** figure specifically - no prior idle
    /// time. Task 6's own first submission (before any fix round) decoded
    /// the previous commit's amplitude 1e-6 before carrier gating existed
    /// at all; gating on a fixed-constant floor cut that to 0.0075. This
    /// particular number has not moved since across any later fix -
    /// freezing the floor on detection and, later, clamping how far it can
    /// decay both leave it untouched, because the cold-start race is
    /// decided by `level` against `INITIAL_FLOOR` in the first attack time
    /// constant, milliseconds, which is over long before `FLOOR_ADAPT`
    /// (1.25 s) can matter either way. (The cold-start noise *ceiling* did
    /// move across fix rounds, from about 0.012 to about 0.0205 - see
    /// `noise_on_a_dead_line_produces_no_bytes` - but that is a different
    /// number from this test's sensitivity figure, decided by a different
    /// side of the same race.)
    ///
    /// Sensitivity **does** improve with idle time - see `floor_adapts_
    /// downward_during_a_quiet_settle_period`, which is the other half of
    /// this property and was previously, wrongly, presented as
    /// contradicting this one. Both are true simultaneously: 0.0075 at a
    /// cold start, and about 0.0006 after 10 s or more of idle line - see
    /// `carrier.rs`'s module doc for the full settle-time table. Neither
    /// is "the" structural sensitivity floor; both are readings of the
    /// same mechanism at different idle durations.
    #[test]
    fn carrier_detection_sensitivity_window() {
        fn clean_loopback_detects(amplitude: f32) -> bool {
            let c = cfg(Role::Originate, 8000);
            let mut tx = Tx::new(c);
            let mut rx = Rx::new(c);
            tx.write(b"CONNECT 300");
            let mut buf = vec![0.0f32; 733];
            let mut got = [0u8; 256];
            let mut sent = 0usize;
            let total = 8000usize + 11 * 10 * 8000 / 300;
            while sent < total {
                tx.read(&mut buf);
                for v in buf.iter_mut() {
                    *v *= amplitude;
                }
                rx.write(&buf);
                rx.read(&mut got);
                sent += 733;
            }
            rx.carrier_detected()
        }

        assert!(
            clean_loopback_detects(0.0075),
            "amplitude 0.0075 must still raise carrier - sensitivity regressed"
        );
        assert!(
            !clean_loopback_detects(0.005),
            "amplitude 0.005 unexpectedly raised carrier - update the documented \
             boundary in this test and in carrier.rs if this is an intentional improvement"
        );
    }

    /// Proves the floor actually adapts while undetected, rather than
    /// sitting at `INITIAL_FLOOR` forever. Mutation-proven against
    /// `FLOOR_ADAPT = 0.0`: Task 6's fix round 1 shipped a floor that
    /// only ever moved by an amount so small none of that round's tests
    /// noticed, and the reviewer proved it by setting its equivalent
    /// constant to zero and watching the full suite pass unchanged. A
    /// receiver on a genuinely quiet line for a couple of seconds should
    /// become *more* sensitive to a weak signal than a receiver that has
    /// only just started, because the floor has had time to settle below
    /// `INITIAL_FLOOR`'s starting guess - amplitude 0.003 is below this
    /// crate's measured 0.0075 sensitivity floor from a cold start, but
    /// clears it after two seconds of quiet.
    ///
    /// The same downward adaptation this test wants is exactly what let
    /// fix round 1's floor decay towards zero given enough quiet, with
    /// nothing to stop it - the fix for that (`FLOOR_MIN_FRACTION`, see
    /// `carrier.rs`) is a lower clamp, not a removal of this behaviour,
    /// so this test and `quiet_then_noise_does_not_fabricate_bytes` are
    /// two sides of the same mechanism and both have to keep passing.
    #[test]
    fn floor_adapts_downward_during_a_quiet_settle_period() {
        let c = cfg(Role::Originate, 8000);
        let mut rx = Rx::new(c);

        let quiet = vec![0.0f32; 733];
        let mut sent = 0;
        while sent < 16_000 {
            // 2 s settle, well clear of FLOOR_ADAPT's 1.25 s time constant.
            rx.write(&quiet);
            sent += 733;
        }

        let mut tx = Tx::new(c);
        tx.write(b"CONNECT 300");
        let mut buf = vec![0.0f32; 733];
        let mut got = [0u8; 256];
        let mut sent = 0usize;
        let total = 8000usize + 11 * 10 * 8000 / 300;
        while sent < total {
            tx.read(&mut buf);
            for v in buf.iter_mut() {
                *v *= 0.003; // below the cold-start sensitivity floor
            }
            rx.write(&buf);
            rx.read(&mut got);
            sent += 733;
        }
        assert!(
            rx.carrier_detected(),
            "a signal too weak for a cold start must still raise carrier \
             after the floor has had time to settle on a quiet line"
        );
    }

    /// The combination nothing before this test covered, and the one that
    /// would have caught fix round 1's regression: `noise_on_a_dead_line_
    /// produces_no_bytes` starts noise immediately on a fresh `Rx`, where
    /// the floor is still at `INITIAL_FLOOR`, and `floor_adapts_downward_
    /// during_a_quiet_settle_period` follows quiet with a weak *signal*,
    /// never with noise. With no lower bound on the floor, a few seconds
    /// of quiet let it decay far enough that noise more than a decade
    /// quieter than this crate's -38 dBFS mic-noise reference asserted
    /// carrier and stayed locked for as long as that noise continued.
    /// This is a half-duplex modem: a gap of a few seconds between
    /// transmissions is the ordinary operating condition, not an edge
    /// case.
    #[test]
    fn quiet_then_noise_does_not_fabricate_bytes() {
        let c = cfg(Role::Originate, 8000);
        let mut rx = Rx::new(c);
        let quiet = vec![0.0f32; 733];
        let mut sent = 0;
        while sent < 80_000 {
            // 10 s settle - the regression measured about 5 s.
            rx.write(&quiet);
            sent += 733;
        }
        let noise = noise_buf(1e-3, 40_000, 0x2545F491_4F6CDD1D);
        let mut got = [0u8; 256];
        let mut total = 0;
        for chunk in noise.chunks(733) {
            rx.write(chunk);
            total += rx.read(&mut got);
        }
        assert_eq!(
            total, 0,
            "fabricated {total} bytes from amplitude 1e-3 noise after a 10 s quiet settle"
        );
        assert_eq!(
            rx.framing_errors(),
            0,
            "fabricated framing errors from amplitude 1e-3 noise after a 10 s quiet settle"
        );
    }

    /// Pins noise rejection at this crate's 1e-3 reference amplitude
    /// across a range of idle durations rather than at one point - the
    /// floor's lower clamp has to hold regardless of how long the line
    /// sat quiet first, which is exactly the property a single settle
    /// duration cannot demonstrate.
    #[test]
    fn noise_rejection_at_reference_amplitude_holds_across_idle_durations() {
        for settle_secs in [0.0f64, 1.0, 5.0, 10.0, 60.0] {
            let c = cfg(Role::Originate, 8000);
            let mut rx = Rx::new(c);
            let quiet = vec![0.0f32; 733];
            let settle_samples = (settle_secs * 8000.0) as usize;
            let mut sent = 0;
            while sent < settle_samples {
                rx.write(&quiet);
                sent += 733;
            }
            let noise = noise_buf(1e-3, 40_000, 0x2545F491_4F6CDD1D);
            let mut got = [0u8; 256];
            let mut total = 0;
            for chunk in noise.chunks(733) {
                rx.write(chunk);
                total += rx.read(&mut got);
            }
            assert_eq!(
                total, 0,
                "settle {settle_secs}s: fabricated {total} bytes from amplitude 1e-3 noise"
            );
            assert_eq!(
                rx.framing_errors(),
                0,
                "settle {settle_secs}s: fabricated framing errors from amplitude 1e-3 noise"
            );
        }
    }

    /// Corrects a claim from this crate's own history. A false lock from
    /// noise is not permanent - it clears once the noise that caused it
    /// actually stops, in about the hold-off period. "Never recovers" (an
    /// earlier report's wording) meant "while the same noise continues",
    /// which is materially more benign than it reads.
    #[test]
    fn a_false_lock_clears_once_the_noise_actually_stops() {
        let c = cfg(Role::Originate, 8000);
        let mut rx = Rx::new(c);
        // Loud enough to falsely lock on immediately from a cold start.
        let noise = noise_buf(0.03, 80_000, 0x2545F491_4F6CDD1D);
        for chunk in noise.chunks(733) {
            rx.write(chunk);
        }
        assert!(
            rx.carrier_detected(),
            "amplitude 0.03 should falsely lock on for this test to mean anything"
        );

        // Real silence, not noise: the false lock must clear within a
        // couple of hold-off periods once its cause is actually gone.
        let quiet = vec![0.0f32; 733];
        let mut cleared = false;
        for _ in 0..20 {
            // ~1.8 s, well over one 0.5 s hold-off.
            rx.write(&quiet);
            if !rx.carrier_detected() {
                cleared = true;
                break;
            }
        }
        assert!(
            cleared,
            "false lock did not clear within 1.8 s of real silence"
        );
    }
}
