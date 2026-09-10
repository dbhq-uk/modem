//! Streaming FSK modulator.

use alloc::collections::VecDeque;
#[cfg(test)]
use alloc::vec;
use alloc::vec::Vec;

use crate::frame::frame_byte;
use crate::nco::Nco;
use crate::resample::Resampler;
use crate::{samples_per_symbol, tones, Config, DSP_RATE};

/// Bound on the pending output queue - device-rate samples the resampler
/// produced in a previous `read` but that call's `out` was already full.
///
/// Round 1 review finding: this was 32 with a comment claiming it "keeps
/// it from ever reallocating", which was not measured and was wrong. The
/// per-call surplus is bounded by roughly 2 * (device_rate / DSP_RATE);
/// measured over 5000 duplex quanta, that held within capacity (zero
/// reallocations) at 8000/44100/48000/96000 Hz but overflowed 32 - one
/// reallocation - at 176400 and 192000 Hz, both real cpal-reportable rates
/// (Task 13). 64 comfortably covers the worst case in this crate's actual
/// range (about 48 at 192 kHz) without claiming a guarantee this constant
/// cannot actually give for an arbitrary future device rate.
const PENDING_CAP: usize = 64;

/// Write queues bytes; read fills sample blocks. Stateful: the symbol
/// clock, the oscillator phase and the resampler's own history all carry
/// across calls, so blocks must be read in order.
///
/// The scratch buffers are owned and grown once, never per call.
/// Allocating inside an audio callback is how you get dropouts.
pub struct Tx {
    cfg: Config,
    nco: Nco,
    mark: f64,
    space: f64,
    bits: VecDeque<bool>,
    sym_pos: f64,
    current: bool,
    scratch: Vec<f64>,
    resampler: Resampler,
    /// Resampler output for the current batch of `scratch`, at DSP_RATE
    /// resampled to the device rate. Reused, resized only on growth.
    resample_out: Vec<f64>,
    /// Device-rate samples the resampler produced but `read`'s caller
    /// buffer was already full for. Drained before generating anything
    /// new, so no produced sample is ever dropped.
    pending: VecDeque<f32>,
    /// Output scale, 1.0 unless deliberately turned down. See
    /// [`Tx::set_amplitude`].
    amplitude: f32,
}

impl Tx {
    pub fn new(cfg: Config) -> Self {
        assert!(cfg.sample_rate > 0, "sample_rate must be greater than zero");
        let (mark, space) = tones(cfg.role);
        Self {
            cfg,
            nco: Nco::new(mark, DSP_RATE),
            mark,
            space,
            bits: VecDeque::new(),
            sym_pos: 0.0,
            current: true, // idle holds mark
            scratch: Vec::new(),
            resampler: Resampler::new(DSP_RATE, cfg.sample_rate as f64),
            resample_out: Vec::new(),
            pending: VecDeque::with_capacity(PENDING_CAP),
            amplitude: 1.0,
        }
    }

    pub fn write(&mut self, data: &[u8]) {
        for &b in data {
            self.bits.extend(frame_byte(b));
        }
    }

    /// Queues `n` bit periods of idle mark.
    ///
    /// A `Tx` whose queue is empty already holds mark, so this is not
    /// about the tone - it is about *reserving time* in the queue, so
    /// that whatever a caller appends next cannot start until the mark
    /// has actually been held for `n` bits. That is the difference
    /// between "there will be a gap if nobody says anything" and "there
    /// is a gap", and a receiver's deframer needs the second one: it has
    /// to see sustained mark before it can read the next space as a
    /// start bit rather than as data.
    ///
    /// Used by `Session::grant_turn` - see its own doc and this module's
    /// note on why every burst needs a preamble *and* a gap after it.
    pub fn write_idle_mark(&mut self, n: usize) {
        self.bits.extend(core::iter::repeat_n(true, n));
    }

    pub fn pending(&self) -> bool {
        !self.bits.is_empty()
    }

    /// Scales everything this `Tx` emits.
    ///
    /// Exists for one caller: `Session`'s `idle_tx`, the transmitter that
    /// holds continuous mark while this end does not have the turn. On a
    /// telephone line that tone never returns to this end's own receiver.
    /// Over a room it does, loudly - a device's own loudspeaker is inches
    /// from its own microphone and the far device is 10-20cm away - so at
    /// full scale it is the loudest thing this end's receiver has to see
    /// past, and it is self-inflicted. `modem-core/tests/acoustic.rs`
    /// measures what that costs; `IDLE_MARK_AMPLITUDE` in `session.rs`
    /// carries the value chosen and why.
    ///
    /// Safe to turn a long way down as far as carrier goes: detection is
    /// a *ratio* test (`carrier.rs`'s `ToneDominance` compares narrowband
    /// against wideband energy), so scaling the tone scales both sides
    /// and leaves the ratio alone - measured still detecting at 0.02.
    /// What it is *not* safe to do is step the level inside a burst; see
    /// `TAIL_IDLE_GAP_BITS`.
    pub fn set_amplitude(&mut self, amplitude: f32) {
        self.amplitude = amplitude;
    }

    /// How many samples `n` bits occupy at this `Tx`'s configured rate -
    /// `round(n * samples_per_symbol())`. A duration conversion, not a
    /// layout tool: a fresh `Tx` idles on mark and only retunes at a symbol
    /// boundary *after* that boundary's sample has already gone out (see
    /// `next_symbol` and `read`'s loop), so the first symbol period of any
    /// transmission is always idle mark rather than the first queued bit.
    /// `samples_for_bits(n)` on a `Tx` that has just had `n` bits queued
    /// therefore spans that one-symbol lead-in plus only `n - 1` of the
    /// queued bits - the last one lands in whatever comes next.
    ///
    /// Stacking several stages back-to-back by adding up
    /// `samples_for_bits` calls for each one's bit count is exactly the
    /// mistake this leads to: every stage after the first loses its final
    /// bit into the following stage's opening samples. This function
    /// cannot be used to lay out a multi-stage sequence exactly for that
    /// reason - a sequencer needs to either drain each `Tx` until
    /// `pending()` is false (plus one more symbol to flush the last bit)
    /// or account for the lead-in itself.
    pub fn samples_for_bits(&self, n: usize) -> usize {
        libm::round(n as f64 * samples_per_symbol()) as usize
    }

    /// Fills `out` with the next block at the device rate, holding mark when
    /// idle. Allocates only on the first call, or if the block size grows.
    ///
    /// Drains `pending` first, then generates and resamples DSP-rate
    /// batches until `out` is full. A batch almost always produces at
    /// least as many device-rate samples as `out` still needs (the sizing
    /// below asks for a small margin over the naive estimate); the loop
    /// exists so that is a performance property, not a correctness one -
    /// if a batch ever falls short, the next iteration just asks for more,
    /// and if it produces extra, the remainder carries to `pending` for the
    /// next call rather than being dropped. Either way every DSP-rate
    /// sample generated corresponds to exactly one device-rate sample
    /// delivered somewhere, which is what keeps the resampler's fractional
    /// phase meaningful across calls.
    pub fn read(&mut self, out: &mut [f32]) {
        let mut written = 0;
        while written < out.len() {
            if let Some(v) = self.pending.pop_front() {
                out[written] = v;
                written += 1;
                continue;
            }

            let remaining = out.len() - written;
            let need =
                libm::ceil(remaining as f64 * DSP_RATE / self.cfg.sample_rate as f64) as usize + 1;
            if self.scratch.len() < need {
                self.scratch.resize(need, 0.0);
            }

            // Derived from samples_per_symbol so there is exactly one
            // implementation of the 26.6667 arithmetic. A second inline
            // copy would not be covered by that function's test, and a
            // rounding regression here would ship green.
            let step = 1.0 / samples_per_symbol();

            for i in 0..need {
                self.scratch[i] = self.nco.next();
                self.sym_pos += step;
                if self.sym_pos >= 1.0 {
                    self.sym_pos -= 1.0;
                    self.next_symbol();
                }
            }

            let max_out = self.resampler.max_output_len(need);
            if self.resample_out.len() < max_out {
                self.resample_out.resize(max_out, 0.0);
            }
            let produced = self
                .resampler
                .process(&self.scratch[..need], &mut self.resample_out[..max_out]);

            for &v in &self.resample_out[..produced] {
                let v = v as f32 * self.amplitude;
                if written < out.len() {
                    out[written] = v;
                    written += 1;
                } else {
                    self.pending.push_back(v);
                }
            }
        }
    }

    fn next_symbol(&mut self) {
        self.current = self.bits.pop_front().unwrap_or(true); // idle mark
        self.nco
            .set_freq(if self.current { self.mark } else { self.space });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nco::goertzel;
    use crate::{Duplex, Role};

    fn cfg(role: Role) -> Config {
        Config {
            sample_rate: 8000,
            role,
            duplex: Duplex::HalfPingPong,
        }
    }

    fn f64s(v: &[f32]) -> Vec<f64> {
        v.iter().map(|&x| x as f64).collect()
    }

    #[test]
    fn tones_per_role() {
        assert_eq!(tones(Role::Originate), (1270.0, 1070.0));
        assert_eq!(tones(Role::Answer), (2225.0, 2025.0));
    }

    #[test]
    fn idle_transmitter_holds_mark() {
        let mut tx = Tx::new(cfg(Role::Originate));
        let mut buf = vec![0.0f32; 8000];
        tx.read(&mut buf);
        assert!(goertzel(&f64s(&buf), 1270.0, 8000.0) >= 0.9);
    }

    /// The critical timing property. Over 1000 bits, rounding to 27 samples
    /// per symbol puts the last bit 333 samples late - more than twelve
    /// whole symbols.
    #[test]
    fn samples_for_bits_is_fractional() {
        let tx = Tx::new(cfg(Role::Originate));
        assert_eq!(tx.samples_for_bits(1000), 26_667);
    }

    /// Guards the live path. samples_for_bits has its own test above, but
    /// read() carries its own symbol accumulator and only this covers it.
    /// Mutation-proven against a rounded step.
    #[test]
    fn read_symbol_clock_drains_bits_at_the_right_rate() {
        let mut tx = Tx::new(cfg(Role::Originate));
        const CHARS: usize = 200;
        tx.write(&[0x55; CHARS]);
        let queued = CHARS * 10;

        let total = tx.samples_for_bits(queued);
        let mut buf = vec![0.0f32; total];
        tx.read(&mut buf);

        let drained = queued - tx.bits.len();
        assert!(
            drained >= queued - 5,
            "drained {drained} of {queued} queued bits, symbol clock is drifting"
        );
    }

    #[test]
    fn write_then_read_produces_both_tones() {
        let mut tx = Tx::new(cfg(Role::Originate));
        tx.write(&[0x00]);
        let mut buf = vec![0.0f32; 800];
        tx.read(&mut buf);
        let s = f64s(&buf);
        assert!(goertzel(&s, 1270.0, 8000.0) >= 0.2, "no mark tone");
        assert!(goertzel(&s, 1070.0, 8000.0) >= 0.2, "no space tone");
    }

    #[test]
    fn read_does_not_allocate_after_the_first_call() {
        let mut tx = Tx::new(cfg(Role::Originate));
        let mut buf = vec![0.0f32; 512];
        tx.read(&mut buf);
        // Compare the address, not the capacity. A fresh allocation of the
        // same size reports the same capacity, so a capacity check passes
        // against precisely the bug it exists to catch.
        //
        // Check after every call, not just first vs last. The system
        // allocator's free list ping-pongs between two addresses on
        // repeated same-size alloc-then-free-old (each call allocates the
        // new buffer before dropping the old one, freeing an address the
        // very next call's allocation then reuses), so an endpoint-only
        // comparison across an even number of calls coincidentally lands
        // back on the starting address even though every call in between
        // reallocated. Verified: with a `Vec::with_capacity(need)` mutant
        // reintroduced on every call, comparing only before and after 50
        // iterations passed, but every one of those 50 calls had in fact
        // moved to a fresh address.
        let ptr = tx.scratch.as_ptr();
        for i in 0..50 {
            tx.read(&mut buf);
            assert_eq!(
                tx.scratch.as_ptr(),
                ptr,
                "scratch was reallocated in the hot path (call {i})"
            );
        }
    }

    /// Round 1 review finding: the test above runs at 8000 Hz, which is
    /// the resampler's identity bypass - it cannot see whether the
    /// batching loop `read` grew to drive a real resampler (the `scratch`
    /// resize, the `resample_out` resize, or the `pending` queue) ever
    /// allocates in its own hot path. This drives the same check through
    /// an actual 48 kHz interpolation.
    #[test]
    fn read_does_not_allocate_after_the_first_call_at_48khz() {
        let mut tx = Tx::new(Config {
            sample_rate: 48000,
            role: Role::Originate,
            duplex: Duplex::HalfPingPong,
        });
        let mut buf = vec![0.0f32; 512];
        tx.read(&mut buf);
        let ptr = tx.scratch.as_ptr();
        for i in 0..50 {
            tx.read(&mut buf);
            assert_eq!(
                tx.scratch.as_ptr(),
                ptr,
                "scratch was reallocated in the hot path at 48 kHz (call {i})"
            );
        }
    }

    /// Bits must reach the wire in the order they were written.
    ///
    /// Aggregate checks cannot see this: a drained-bit count is identical
    /// under any consumption order, and tone-presence is identical too.
    /// Per-symbol Goertzel does not help either - at 26.6667 samples per
    /// symbol, 1270 Hz and 1070 Hz both fall in the same bin, so the
    /// frequency domain cannot resolve one symbol at this window length.
    ///
    /// So compare against an independent oracle that mirrors the intended
    /// state machine and diff the raw samples.
    #[test]
    fn read_emits_bits_in_write_order() {
        let mut tx = Tx::new(cfg(Role::Originate));
        let payload = [0x41u8, 0x7E];
        tx.write(&payload);

        let total = tx.samples_for_bits(payload.len() * 10);
        let mut got = vec![0.0f32; total];
        tx.read(&mut got);

        let (mark, space) = tones(Role::Originate);
        let mut bits: Vec<bool> = Vec::new();
        for &b in &payload {
            bits.extend(frame_byte(b));
        }

        let mut nco = crate::nco::Nco::new(mark, DSP_RATE);
        let mut sym_pos = 0.0f64;
        let mut idx = 0usize;
        let step = 1.0 / samples_per_symbol();
        let mut want = vec![0.0f32; total];
        for w in want.iter_mut() {
            *w = nco.next() as f32;
            sym_pos += step;
            if sym_pos >= 1.0 {
                sym_pos -= 1.0;
                let bit = bits.get(idx).copied().unwrap_or(true);
                idx += 1;
                nco.set_freq(if bit { mark } else { space });
            }
        }

        let mse: f64 = got
            .iter()
            .zip(&want)
            .map(|(a, b)| {
                let d = (*a - *b) as f64;
                d * d
            })
            .sum::<f64>()
            / total as f64;
        assert!(
            mse < 1e-12,
            "waveform diverges from the oracle, mse = {mse:e}"
        );
    }
}
