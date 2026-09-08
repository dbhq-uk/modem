//! Rate conversion between the device and the 8 kHz internal rate.
//!
//! A stateful, band-limited resampler. It fixes the two defects of Task 3's
//! placeholder:
//!
//! 1. **No anti-aliasing.** Content above 4 kHz used to fold into band on
//!    the way down; a Blackman-windowed sinc low-pass at 3600 Hz now runs
//!    before decimation and after interpolation, so neither direction
//!    folds.
//! 2. **Stateless.** The fractional phase, the filter history and the
//!    small interpolation window all persist across calls now, so
//!    consecutive blocks join sample-for-sample rather than each restarting
//!    from position zero.
//!
//! # Where the filter runs, and why 63 taps is not a fixed sample count
//!
//! The cutoff (3600 Hz) is chosen relative to the *internal* rate's Nyquist
//! (4 kHz) - see [`CUTOFF_HZ`]. But the filter itself must run on whichever
//! side of a conversion is the higher rate: for decimation it has to see
//! the input before the rate drops, because once a sample has been taken at
//! the lower rate, there is no way to tell "aliased 6 kHz" apart from "real
//! 2 kHz" - they are the same number. For interpolation it has to run after
//! the upsampling step, to remove the images that step creates.
//!
//! That leaves a choice: what does "63 taps" mean when the higher rate
//! varies (8 kHz for Tx's own device, 44100 or 48000 Hz for a sound card)?
//! Measured directly (`docs` below cites the numbers): a literal 63-sample
//! convolution evaluated at 48 kHz spans only 1.3 ms, which is far too
//! short a filter for a 3400-4000 Hz transition band at that rate - a 3 kHz
//! tone measured at 0.795 of its input magnitude after "decimation",
//! nowhere near the "full magnitude" the design requires. Fixing the
//! *physical* span instead - 62 sample periods at the 8 kHz reference rate,
//! about 7.75 ms - and evaluating that same continuous kernel more densely
//! when the filter has to run at a higher rate keeps the cutoff and
//! transition band fixed in absolute Hz regardless of what rate is
//! actually driving it. Measured with that fix: the same 3 kHz tone at
//! 48 kHz survives at 0.99996, and 44100 Hz gives 0.99954. `REF_TAPS = 63`
//! is this filter's length only when it is evaluated at exactly 8 kHz
//! (which happens not to arise in practice, since Tx and Rx never both sit
//! at 8 kHz and still need a real filter - that combination is the
//! identity case and bypasses filtering entirely); at 48 kHz the same
//! kernel has 373 coefficients, at 44100 Hz it has 343. All are computed
//! once at construction, never reallocated afterward.
//!
//! # Order of operations
//!
//! - Decimating (`src_rate > dst_rate`): filter the raw input first, at
//!   `src_rate`, then cubic-Hermite-interpolate the filtered signal down to
//!   `dst_rate`.
//! - Interpolating (`src_rate < dst_rate`): cubic-Hermite-interpolate the
//!   raw input up to `dst_rate` first, then filter at `dst_rate`.
//! - Equal rates: bypass both stages entirely. Nothing needs removing, and
//!   this keeps every existing 8 kHz test bit-exact rather than routing it
//!   through a filter with its own group delay and rounding.
//!
//! # Cubic Hermite over linear
//!
//! Once the signal is band-limited, either interpolation scheme reproduces
//! it well *if the sampling is generous relative to the content* - which is
//! exactly the case on the decimating (Rx) side, where the content tops out
//! at 3600 Hz on a signal sampled at the device's full rate. It is not the
//! case on the interpolating (Tx) side: Bell 103's own tones (1070-2225 Hz)
//! sit at a third to over half of the *internal* rate's Nyquist, which is
//! the rate Tx's interpolation step actually runs at before the filter ever
//! sees the signal. Measured directly there (Goertzel, integer-cycle
//! windows, both through the real filter+interpolate pipeline): a 1270 Hz
//! mark tone survives Hermite interpolation at 0.9887 of its magnitude and
//! linear interpolation at only 0.9219; a 2225 Hz tone survives at 0.9114
//! with Hermite and 0.7755 with linear. This module's own
//! `hermite_beats_linear_on_bell_103_tones` test pins this - see Mutation 4
//! in the task report for the full sweep.
//!
//! Nothing here allocates outside [`Resampler::new`]. The filter history
//! and the four-point interpolation window are sized once at construction
//! and never resized.

use alloc::vec;
use alloc::vec::Vec;
use libm::{cos, round, sin};

use crate::DSP_RATE;

/// Low-pass cutoff, in Hz: above the 3400 Hz telephone band and below the
/// 4 kHz Nyquist of the internal rate.
const CUTOFF_HZ: f64 = 3600.0;

/// The filter's reference length, defined at DSP_RATE. This pins the
/// filter's physical time span - (63 - 1) / 8000 = 7.75 ms - rather than
/// its raw sample count; see the module doc for why a literal 63-sample
/// convolution evaluated directly at a higher rate is the wrong thing.
const REF_TAPS: usize = 63;

/// Builds a Blackman-windowed sinc low-pass with the physical span fixed by
/// [`REF_TAPS`] at [`DSP_RATE`], evaluated at `filter_fs` (which may be a
/// device rate higher than DSP_RATE, giving more than `REF_TAPS`
/// coefficients - see the module doc). DC gain is normalised to 1.
fn design_lowpass(filter_fs: f64) -> Vec<f64> {
    let span_s = (REF_TAPS - 1) as f64 / DSP_RATE;
    // Guards against a degenerate design if this is ever asked to run
    // below twice the cutoff; not reachable with this crate's rates
    // (device rates are always at least 8000 Hz), kept as a defensive
    // clamp rather than a documented behaviour.
    let cutoff = CUTOFF_HZ.min(filter_fs * 0.49);
    let n = round(span_s * filter_fs) as usize + 1;
    let m = (n - 1) as f64;
    let mut taps = vec![0.0f64; n];
    for (i, tap) in taps.iter_mut().enumerate() {
        let t = i as f64 / filter_fs - span_s / 2.0;
        let s = if t.abs() < 1e-12 {
            2.0 * cutoff
        } else {
            sin(core::f64::consts::TAU * cutoff * t) / (core::f64::consts::PI * t)
        };
        let x = i as f64 / m;
        let w = 0.42 - 0.5 * cos(core::f64::consts::TAU * x)
            + 0.08 * cos(2.0 * core::f64::consts::TAU * x);
        *tap = s * w;
    }
    let sum: f64 = taps.iter().sum();
    for tap in taps.iter_mut() {
        *tap /= sum;
    }
    taps
}

/// Four-point cubic Hermite (Catmull-Rom) interpolation between `y1` and
/// `y2`, using `y0` and `y3` as the neighbours that set the tangents. `t`
/// is the fractional position in `[0, 1)` between `y1` and `y2`.
fn hermite(y0: f64, y1: f64, y2: f64, y3: f64, t: f64) -> f64 {
    let c0 = y1;
    let c1 = 0.5 * (y2 - y0);
    let c2 = y0 - 2.5 * y1 + 2.0 * y2 - 0.5 * y3;
    let c3 = 0.5 * (y3 - y0) + 1.5 * (y1 - y2);
    ((c3 * t + c2) * t + c1) * t + c0
}

/// A stateful, band-limited resampler between two fixed sample rates.
///
/// One instance handles one direction between one pair of rates for its
/// whole life - `Tx` owns one from `DSP_RATE` to the device rate, `Rx` owns
/// one the other way. See the module doc for the filter design and the
/// order of operations.
pub struct Resampler {
    src_rate: f64,
    dst_rate: f64,
    identity: bool,
    decimating: bool,
    /// How far the fractional position advances per output sample:
    /// `src_rate / dst_rate`.
    step: f64,

    taps: Vec<f64>,
    fir_hist: Vec<f64>,
    fir_pos: usize,

    /// The four most recently produced samples feeding the Hermite stage,
    /// oldest first.
    herm_hist: [f64; 4],
    /// Fractional read position, in units of samples fed to the Hermite
    /// stage, carried across calls.
    frac_pos: f64,
    /// Total samples fed to the Hermite stage so far, carried across
    /// calls. f64 keeps exact integer values well past any session length
    /// this crate runs (exact to 2^53).
    n_pushed: f64,
}

impl Resampler {
    pub fn new(src_rate: f64, dst_rate: f64) -> Self {
        assert!(
            src_rate > 0.0 && dst_rate > 0.0,
            "sample rates must be positive"
        );
        let identity = src_rate == dst_rate;
        let decimating = src_rate > dst_rate;
        let filter_fs = if src_rate > dst_rate {
            src_rate
        } else {
            dst_rate
        };
        let taps = if identity {
            Vec::new()
        } else {
            design_lowpass(filter_fs)
        };
        let fir_len = taps.len();
        Self {
            src_rate,
            dst_rate,
            identity,
            decimating,
            step: src_rate / dst_rate,
            taps,
            fir_hist: vec![0.0; fir_len],
            fir_pos: 0,
            herm_hist: [0.0; 4],
            frac_pos: 0.0,
            n_pushed: 0.0,
        }
    }

    /// Returns the resampler to its just-constructed state: filter history,
    /// interpolation window and fractional phase all clear. The filter
    /// coefficients themselves are untouched, since they depend only on the
    /// fixed rates given to `new`.
    pub fn reset(&mut self) {
        for v in self.fir_hist.iter_mut() {
            *v = 0.0;
        }
        self.fir_pos = 0;
        self.herm_hist = [0.0; 4];
        self.frac_pos = 0.0;
        self.n_pushed = 0.0;
    }

    /// A safe upper bound on how many output samples `process` can produce
    /// for `input_len` input samples, for sizing an output buffer. Errs
    /// generous rather than tight.
    pub fn max_output_len(&self, input_len: usize) -> usize {
        if self.identity {
            return input_len;
        }
        round(input_len as f64 * self.dst_rate / self.src_rate) as usize + 2
    }

    /// Pushes one sample through the FIR and returns the filtered value.
    /// The ring buffer's write position is the only state that moves;
    /// nothing here allocates.
    fn fir_push(&mut self, x: f64) -> f64 {
        let n = self.taps.len();
        self.fir_hist[self.fir_pos] = x;
        let mut acc = 0.0;
        let mut idx = self.fir_pos;
        for &c in &self.taps {
            acc += c * self.fir_hist[idx];
            idx = if idx == 0 { n - 1 } else { idx - 1 };
        }
        self.fir_pos += 1;
        if self.fir_pos >= n {
            self.fir_pos = 0;
        }
        acc
    }

    /// Resamples `input` into `output`, consuming all of `input` and
    /// returning how many samples were written. That count varies block to
    /// block at a non-integer ratio - callers must not assume one, and must
    /// size `output` for the worst case (see [`Self::max_output_len`]) since
    /// this stops rather than overruns it if it is too small.
    pub fn process(&mut self, input: &[f64], output: &mut [f64]) -> usize {
        if self.identity {
            let n = input.len().min(output.len());
            output[..n].copy_from_slice(&input[..n]);
            return n;
        }

        let mut out_i = 0;
        for &x in input {
            let herm_in = if self.decimating { self.fir_push(x) } else { x };
            self.herm_hist = [
                self.herm_hist[1],
                self.herm_hist[2],
                self.herm_hist[3],
                herm_in,
            ];
            self.n_pushed += 1.0;

            // Once four real samples surround frac_pos, produce every
            // output that stencil covers before the next input sample
            // shifts the window on. Upsampling drains several per push;
            // decimating drains at most one, and most pushes drain none.
            while self.n_pushed - 2.0 > self.frac_pos {
                if out_i >= output.len() {
                    // The caller under-sized output. Stop rather than
                    // overrun it; the remaining input is not consumed.
                    return out_i;
                }
                let k_base = self.n_pushed - 3.0;
                let t = self.frac_pos - k_base;
                let y = hermite(
                    self.herm_hist[0],
                    self.herm_hist[1],
                    self.herm_hist[2],
                    self.herm_hist[3],
                    t,
                );
                let y = if self.decimating { y } else { self.fir_push(y) };
                output[out_i] = y;
                out_i += 1;
                self.frac_pos += self.step;
            }
        }
        out_i
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nco::goertzel;
    use alloc::vec;

    /// A sine generator that can hand out samples in whatever block sizes
    /// the caller asks for, so the same signal can be produced either in
    /// one call or split awkwardly.
    fn sine(freq: f64, rate: f64, n: usize) -> Vec<f64> {
        (0..n)
            .map(|i| libm::sin(core::f64::consts::TAU * freq * i as f64 / rate))
            .collect()
    }

    /// A Goertzel window sized to an integer number of cycles at `freq`
    /// against `fs`, at least `min_len` long. Goertzel assumes a
    /// periodic window; a non-integer cycle count leaks energy out of the
    /// target bin and understates the true magnitude, which would make an
    /// otherwise-correct resampler look like it was attenuating a tone it
    /// was not. Measured: a 1270 Hz tone through a correct implementation
    /// read 0.82 against an arbitrary 16000-sample window and 0.99 against
    /// this one - the resampler was fine, the window was not.
    fn cycle_window(freq: f64, fs: f64, min_len: usize) -> usize {
        fn gcd(a: u64, b: u64) -> u64 {
            if b == 0 {
                a
            } else {
                gcd(b, a % b)
            }
        }
        let g = gcd(freq as u64, fs as u64);
        let period = (fs as u64 / g) as usize;
        let mut n = period;
        while n < min_len {
            n += period;
        }
        n
    }

    #[test]
    fn identity_when_rates_are_equal() {
        let mut r = Resampler::new(8000.0, 8000.0);
        let input = sine(1270.0, 8000.0, 500);
        let mut output = vec![0.0; 500];
        let n = r.process(&input, &mut output);
        assert_eq!(n, 500);
        assert_eq!(output, input, "identity resampling changed the signal");
    }

    /// The anti-aliasing test the placeholder failed outright: at 8000 Hz
    /// the placeholder degenerates to a passthrough and never exercises
    /// this path at all.
    #[test]
    fn three_khz_survives_decimation_to_8khz_at_full_magnitude() {
        let mut r = Resampler::new(48000.0, 8000.0);
        let n_in = 48000;
        let input = sine(3000.0, 48000.0, n_in);
        let max_out = r.max_output_len(n_in);
        let mut output = vec![0.0; max_out];
        let n = r.process(&input, &mut output[..max_out]);
        let win = cycle_window(3000.0, 8000.0, 4000);
        let mag = goertzel(&output[n - win..n], 3000.0, 8000.0);
        assert!(
            mag >= 0.99,
            "3 kHz survived decimation at only {mag}, not full magnitude"
        );
    }

    /// The failing case named directly in the brief: a 6 kHz tone at
    /// 48 kHz aliases to 2 kHz under the placeholder (2 kHz is inside the
    /// answer band). Un-filtered decimation cannot avoid this - once a
    /// sample has been taken at 8 kHz there is no way to tell "aliased
    /// 6 kHz" from "real 2 kHz" apart, so the low-pass has to run before
    /// the rate drops. See mutation proof 1 in the task report for what
    /// this looks like with that low-pass removed.
    #[test]
    fn six_khz_does_not_alias_to_two_khz() {
        let mut r = Resampler::new(48000.0, 8000.0);
        let n_in = 48000;
        let input = sine(6000.0, 48000.0, n_in);
        let max_out = r.max_output_len(n_in);
        let mut output = vec![0.0; max_out];
        let n = r.process(&input, &mut output[..max_out]);
        let win = cycle_window(2000.0, 8000.0, 4000);
        let mag = goertzel(&output[n - win..n], 2000.0, 8000.0);
        assert!(
            mag < 0.01,
            "6 kHz at 48 kHz appeared at 2 kHz with magnitude {mag}"
        );
    }

    /// Block-boundary continuity at a rate dividing evenly into neither
    /// 8000 nor a power of two. Feeds the same continuous sine through in
    /// deliberately awkward, varying block sizes and compares the
    /// concatenated result sample-for-sample against processing it in one
    /// call.
    ///
    /// A length check alone cannot catch a reset defect: mutating either
    /// the fractional phase or the filter history to clear between calls
    /// still produces the *same number* of output samples (the ratio is
    /// unchanged), just wrong ones at every seam - see mutation proofs 2
    /// and 3 in the task report. This compares values, not just counts.
    #[test]
    fn block_boundary_continuity_at_44100hz() {
        let freq = 1000.0;
        let rate = 44100.0;
        let n_in = 5000;
        let input = sine(freq, rate, n_in);

        let mut one_shot_r = Resampler::new(rate, DSP_RATE);
        let max_out = one_shot_r.max_output_len(n_in);
        let mut one_shot_out = vec![0.0; max_out];
        let one_shot_n = one_shot_r.process(&input, &mut one_shot_out[..max_out]);

        let mut chunked_r = Resampler::new(rate, DSP_RATE);
        let mut chunked_out = Vec::new();
        let sizes = [7usize, 13, 1, 29, 733, 3, 101];
        let mut i = 0;
        let mut size_i = 0;
        while i < input.len() {
            let sz = sizes[size_i % sizes.len()].min(input.len() - i);
            size_i += 1;
            let chunk = &input[i..i + sz];
            let mut buf = vec![0.0; chunked_r.max_output_len(sz)];
            let n = chunked_r.process(chunk, &mut buf);
            chunked_out.extend_from_slice(&buf[..n]);
            i += sz;
        }

        assert_eq!(
            chunked_out.len(),
            one_shot_n,
            "chunked and one-shot produced different output lengths"
        );
        for (i, (a, b)) in chunked_out
            .iter()
            .zip(&one_shot_out[..one_shot_n])
            .enumerate()
        {
            assert_eq!(
                a, b,
                "chunked and one-shot diverge at output sample {i} - a block seam is not sample-continuous"
            );
        }
    }

    /// No allocation after construction. Checked after every call across
    /// many calls, not just first-versus-last: the system allocator's free
    /// list ping-pongs between two addresses on repeated same-size
    /// alloc-then-free, so an endpoint-only comparison across an even
    /// number of calls can coincidentally land back on the starting
    /// address even though every call in between reallocated (the same
    /// failure mode documented against `Tx::scratch` and `Rx::scratch`).
    #[test]
    fn no_allocation_after_construction() {
        let mut r = Resampler::new(48000.0, 8000.0);
        let input = sine(1000.0, 48000.0, 512);
        let mut output = vec![0.0; r.max_output_len(512)];
        r.process(&input, &mut output);
        let ptr = r.fir_hist.as_ptr();
        for i in 0..50 {
            r.process(&input, &mut output);
            assert_eq!(
                r.fir_hist.as_ptr(),
                ptr,
                "filter history was reallocated in the hot path (call {i})"
            );
        }
    }

    /// Justifies cubic Hermite over linear interpolation with a measured
    /// difference, per the task's requirement to show one or use linear
    /// and say so. Once the signal is band-limited, either scheme
    /// reproduces content that is heavily oversampled relative to the
    /// filter cutoff (Rx's decimation side: 3600 Hz content sampled at a
    /// 44100 or 48000 Hz device rate) - measured difference there is in
    /// the fourth decimal place and not worth asserting on. It is not true
    /// on Tx's interpolation side, where Bell 103's own tones sit at a
    /// third to over half of the *internal* rate's Nyquist, which is the
    /// rate the interpolation step runs at before the filter ever sees the
    /// signal. This drives all four Bell 103 tones through the real
    /// filter+interpolate pipeline (Goertzel, integer-cycle windows) and
    /// pins the measured gap between the two schemes.
    #[test]
    fn hermite_beats_linear_on_bell_103_tones() {
        // (frequency, Hermite magnitude floor, linear's actual measured
        // magnitude - linear must stay clearly below the floor Hermite
        // clears, or the two schemes are not meaningfully different here).
        let cases = [
            (1070.0, 0.99, 0.9441),
            (1270.0, 0.98, 0.9219),
            (2025.0, 0.93, 0.8109),
            (2225.0, 0.90, 0.7755),
        ];
        for (freq, hermite_floor, linear_measured) in cases {
            let mut r = Resampler::new(DSP_RATE, 48000.0);
            let n_in = 6000;
            let input = sine(freq, DSP_RATE, n_in);
            let mut output = vec![0.0; r.max_output_len(n_in)];
            let n = r.process(&input, &mut output);
            let win = cycle_window(freq, 48000.0, 8000);
            let mag = goertzel(&output[n - win..n], freq, 48000.0);
            assert!(
                mag >= hermite_floor,
                "{freq} Hz survived Hermite interpolation at only {mag}, below the measured floor"
            );
            assert!(
                mag > linear_measured + 0.01,
                "{freq} Hz's Hermite magnitude {mag} is not clearly above linear's measured {linear_measured} - \
                 the two schemes are not meaningfully different at this frequency"
            );
        }
    }
}
