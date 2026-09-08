//! Channel impairments, and the byte error rate they are measured against.
//!
//! Every function here operates on a whole buffer at once - typically the
//! "air" a `Tx` already produced - and several of them allocate. That is
//! deliberate: none of this runs in the streaming per-block path `Tx` and
//! `Rx` use, where an allocation costs a dropped audio callback. A
//! calibration run processes a few thousand samples once, not a live
//! callback, so the no-allocation discipline that matters elsewhere in
//! this crate does not apply here.
//!
//! `modem-core` has no `rand` and stays `no_std`, so any impairment that
//! needs noise seeds a private xorshift64 from its own `seed` argument
//! rather than sharing global state. A fixed seed reproduces a fixed
//! result, and the seed is exactly the number a bug report needs to
//! quote.
//!
//! # What this is actually for
//!
//! [`measure_ber`] and [`MAX_BYTE_ERROR_RATE`] turn "does it work" into a
//! number. Task 5 found the trap this exists to avoid: a cycle slip can
//! hand back the right byte count and zero framing errors while every
//! byte is wrong. A length check, `contains`, or `ends_with` all pass
//! that receiver anyway - three variants of the same blind spot, all
//! already caught doing exactly this in this project. `measure_ber`
//! therefore compares recovered content against the known payload, never
//! a count, and never a substring search.
//!
//! Task 5 also found that wideband noise barely matters here: byte error
//! rate is flat down to 6 dB SNR across a 4x range of loop gain, because
//! the correlator's own matched-filter processing gain does the
//! rejecting before the slicer ever sees the noise. This module's own
//! AWGN measurements confirm the same flatness (see `docs/ber-
//! calibration.md`) and do not try to manufacture a discriminator out of
//! it. What actually degrades the link is the *correlated* impairments -
//! harmonic distortion, reverb, duplex leak - which corrupt specific
//! frequencies or specific moments rather than spreading energy evenly.
//! Those are what set [`MAX_BYTE_ERROR_RATE`].

use alloc::vec;
use alloc::vec::Vec;
use libm::{cos, log, pow, round, sin, sqrt};

use crate::resample::Resampler;
use crate::DSP_RATE;

/// The release gate. Fixed by this task's measurement run against the
/// commit and toolchain recorded in `docs/ber-calibration.md`, which also
/// carries the method behind every figure. If a later change pushes the
/// measured byte error rate above this under the same methodology, the
/// change made the modem worse - this constant does not move to
/// accommodate it.
///
/// 0.01 (1%) matches Task 19's own independent real-world acceptance
/// figure for the two-laptop desk test, and is anchored to a genuine
/// measurement rather than imported wholesale: sweeping `clock_drift`
/// past rx.rs's documented +/-2.5% Gardner pull-in range, +30,000 ppm
/// (3.0%) measured exactly 0.01 - the first hint of degradation, one
/// step before the loop loses lock outright (35,000 ppm measures over
/// 90%). Every scenario this task found to be clean measured exactly
/// 0.0, not merely low, so 0.01 sits with real margin above "working"
/// and over an order of magnitude below every collapse this task
/// measured.
pub const MAX_BYTE_ERROR_RATE: f64 = 0.01;

// ---------------------------------------------------------------------
// Deterministic noise
// ---------------------------------------------------------------------

/// Advances a private xorshift64 state and returns the next value. Same
/// algorithm `rx.rs`'s own test noise generator uses; kept here as the
/// one real implementation rather than a second copy with its own chance
/// to drift from it.
fn xorshift_next(state: &mut u64) -> u64 {
    let mut x = *state;
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    *state = x;
    x
}

/// Xorshift's state must never be zero - it is a fixed point the update
/// can never leave - so a zero seed is remapped to an arbitrary nonzero
/// constant instead of silently producing an all-zero stream forever.
fn seed_state(seed: u64) -> u64 {
    if seed == 0 {
        0x9E3779B97F4A7C15
    } else {
        seed
    }
}

/// A uniform value in (0, 1], built from the top 53 bits of a xorshift
/// draw. Box-Muller takes a log of this, so it must never land on exactly
/// zero.
fn next_uniform(state: &mut u64) -> f64 {
    let bits = xorshift_next(state) >> 11;
    let u = bits as f64 / (1u64 << 53) as f64;
    1.0 - u
}

/// One standard-normal draw via Box-Muller. Discards the second value the
/// transform produces for free - simplicity over speed, since this runs
/// once per sample in an offline calibration pass, not a live callback.
fn gaussian(state: &mut u64) -> f64 {
    let u1 = next_uniform(state);
    let u2 = next_uniform(state);
    sqrt(-2.0 * log(u1)) * cos(core::f64::consts::TAU * u2)
}

// ---------------------------------------------------------------------
// Impairments
// ---------------------------------------------------------------------

/// White Gaussian noise at a stated SNR, added in place via Box-Muller
/// over a xorshift seeded from `seed`. SNR is measured against `samples`'
/// own RMS power over the whole buffer, so the same `snr_db` means the
/// same relative degradation regardless of how loud the caller's signal
/// happens to be. A silent buffer (zero power) gets no noise added -
/// "signal to noise ratio" is not a meaningful question against silence,
/// and adding noise anyway would manufacture a carrier where the real
/// system has none.
pub fn add_awgn(samples: &mut [f32], snr_db: f64, seed: u64) {
    if samples.is_empty() {
        return;
    }
    let signal_power: f64 = samples
        .iter()
        .map(|&s| {
            let s = s as f64;
            s * s
        })
        .sum::<f64>()
        / samples.len() as f64;
    if signal_power == 0.0 {
        return;
    }
    let noise_power = signal_power / pow(10.0, snr_db / 10.0);
    let noise_std = sqrt(noise_power);
    let mut state = seed_state(seed);
    for s in samples.iter_mut() {
        *s += (gaussian(&mut state) * noise_std) as f32;
    }
}

/// Resamples `samples` as though they had been produced by a sound card
/// running `ppm` parts per million away from `DSP_RATE`, rather than at
/// `DSP_RATE` exactly. Positive `ppm` is a fast clock: it produces more
/// physical samples for the same nominal duration, which is exactly what
/// `Tx` does internally when configured for a device rate above
/// `DSP_RATE` - `ppm = 20_000` (2%) reproduces the same offset `rx.rs`'s
/// own `loopback_tracks_a_two_percent_sample_clock_offset` drives through
/// differing `Config::sample_rate`s, here as a standalone function over
/// an already-rendered buffer instead of a second `Tx`/`Rx` pair.
pub fn clock_drift(samples: &[f32], ppm: f64) -> Vec<f32> {
    let drifted_rate = DSP_RATE * (1.0 + ppm / 1_000_000.0);
    let mut r = Resampler::new(DSP_RATE, drifted_rate);
    let input: Vec<f64> = samples.iter().map(|&s| s as f64).collect();
    let mut out = vec![0.0f64; r.max_output_len(input.len())];
    let n = r.process(&input, &mut out);
    out[..n].iter().map(|&v| v as f32).collect()
}

/// A hard limiter: anything beyond +/-`level` is clamped there. Models an
/// overdriven output stage - a speaker or line-out pushed past its rails.
pub fn clip(samples: &mut [f32], level: f32) {
    let level = level.abs();
    for s in samples.iter_mut() {
        *s = s.clamp(-level, level);
    }
}

/// Adds a second-harmonic component in place: `y = x + amount * x^2`. A
/// pure tone at frequency f produces energy at 2f this way
/// (sin^2(wt) = 0.5 - 0.5*cos(2wt)), which is exactly the mechanism a
/// mildly nonlinear, asymmetric amplifier stage adds - the even-order
/// term in its Taylor expansion around the operating point.
///
/// This is the impairment that matters most in this module. The second
/// harmonic of the 1070 Hz Originate space tone lands at 2140 Hz, 85 Hz
/// from the Answer band's 2225 Hz mark tone - the specific mechanism by
/// which a loud Originate transmitter corrupts the Answer direction. See
/// [`duplex_leak`] and this module's own
/// `loud_originate_harmonic_corrupts_the_answer_direction` test, which
/// proves the mechanism through the real receiver rather than asserting
/// it. Mutation proof 3 replaces the second harmonic with the third
/// (`x^3`, landing near 3210 Hz - outside every Bell 103 band) and shows
/// that test stops seeing any damage at all.
pub fn harmonic_distortion(samples: &mut [f32], amount: f64) {
    for s in samples.iter_mut() {
        let x = *s as f64;
        *s = (x + amount * x * x) as f32;
    }
}

/// Physical span, in samples at `DSP_RATE`, for this module's own
/// bandpass filter. Deliberately independent of `resample::REF_TAPS`:
/// that constant is tuned for a 3600 Hz anti-aliasing edge with roughly a
/// 400 Hz transition band, but resolving a 300 Hz lower edge with a
/// comparably tight transition needs meaningfully more taps - Blackman
/// transition width scales as roughly `5.5 * rate / taps` regardless of
/// where the cutoff sits, so the same tap count that suffices at 3600 Hz
/// would leave the 300 Hz edge barely defined at all.
const BAND_LIMIT_REF_TAPS: usize = 255;
const TELEPHONE_LOW_HZ: f64 = 300.0;
const TELEPHONE_HIGH_HZ: f64 = 3400.0;

/// A Blackman-windowed sinc lowpass at `cutoff_hz`, evaluated at
/// `filter_fs`, DC-normalised to unit gain.
///
/// Deliberately not shared with `resample::design_lowpass`: that function
/// is wired to the fixed `CUTOFF_HZ` and `REF_TAPS` constants chosen for
/// a different job (anti-aliasing at the internal rate's Nyquist), and
/// duplicating the ~15 lines of Blackman-sinc maths here keeps this
/// module's own tap count and cutoffs free to move without touching the
/// heavily mutation-tested resampler.
fn design_lowpass_at(cutoff_hz: f64, filter_fs: f64) -> Vec<f64> {
    let span_s = (BAND_LIMIT_REF_TAPS - 1) as f64 / DSP_RATE;
    let n = round(span_s * filter_fs) as usize + 1;
    let m = (n - 1) as f64;
    let mut taps = vec![0.0f64; n];
    for (i, tap) in taps.iter_mut().enumerate() {
        let t = i as f64 / filter_fs - span_s / 2.0;
        let s = if t.abs() < 1e-12 {
            2.0 * cutoff_hz
        } else {
            sin(core::f64::consts::TAU * cutoff_hz * t) / (core::f64::consts::PI * t)
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

/// A telephone bandpass (300 to 3400 Hz), built as the difference of two
/// unit-DC-gain lowpasses: `lowpass(3400) - lowpass(300)` passes whatever
/// sits between the two cutoffs and rejects both DC and everything above
/// the upper edge, without a separate bandpass design.
fn design_bandpass(rate: f64) -> Vec<f64> {
    let hi = design_lowpass_at(TELEPHONE_HIGH_HZ, rate);
    let lo = design_lowpass_at(TELEPHONE_LOW_HZ, rate);
    hi.iter().zip(&lo).map(|(h, l)| h - l).collect()
}

/// Filters `samples` to the 300-3400 Hz telephone passband at `rate`, in
/// place. Convolves the whole buffer against a filter starting from
/// rest, the same way a freshly constructed `Rx` sees zero history for
/// its first samples.
pub fn band_limit(samples: &mut [f32], rate: f64) {
    let taps = design_bandpass(rate);
    let n = samples.len();
    let mut out = vec![0.0f64; n];
    for (i, o) in out.iter_mut().enumerate() {
        let max_k = taps.len().min(i + 1);
        let mut acc = 0.0;
        for (k, &c) in taps.iter().enumerate().take(max_k) {
            acc += c * samples[i - k] as f64;
        }
        *o = acc;
    }
    for (s, &o) in samples.iter_mut().zip(&out) {
        *s = o as f32;
    }
}

/// Adds one delayed reflection - the dominant room effect at desk
/// distances, where the direct path and a single early reflection (desk
/// or wall) dominate over any diffuse tail that would need a full
/// reverberation model. Feed-forward, not feedback: one echo, not a
/// decaying series. A `delay_samples` at or beyond the buffer's length
/// never lands inside it, so the buffer is left untouched rather than
/// panicking on an out-of-range index.
pub fn reverb(samples: &mut [f32], delay_samples: usize, gain: f32) {
    if delay_samples == 0 || delay_samples >= samples.len() {
        return;
    }
    let original = samples.to_vec();
    for i in delay_samples..samples.len() {
        samples[i] += gain * original[i - delay_samples];
    }
}

/// Mixes `near` in under `far`, as an acoustic full-duplex setup does: a
/// microphone hears its own loudspeaker's output leaking back in on top
/// of whatever the far end is actually sending. Whether acoustic full
/// duplex works at all comes down to how much of `near` a receiver
/// decoding `far` can tolerate - see [`harmonic_distortion`] for the
/// specific frequency-domain mechanism this crate cares most about.
///
/// Returns a buffer the length of `far`. Missing `near` samples (it is
/// shorter than `far`) leak nothing at those positions; any of `near`
/// beyond `far`'s length is discarded, since there is nothing left in
/// `far` for it to land on.
pub fn duplex_leak(far: &[f32], near: &[f32], near_gain: f32) -> Vec<f32> {
    far.iter()
        .enumerate()
        .map(|(i, &f)| f + near_gain * near.get(i).copied().unwrap_or(0.0))
        .collect()
}

// ---------------------------------------------------------------------
// Byte error rate
// ---------------------------------------------------------------------

/// How many leading bytes of `recovered` [`measure_ber`] may skip when
/// aligning it to `payload`. A handful covers a training artefact left
/// over from harness bookkeeping, or a slip right at the acquisition
/// boundary; searching further risks matching on the payload's own
/// repetition instead of a genuine alignment.
const ALIGN_SEARCH: usize = 8;

/// Byte error rate between a known `payload` and what a receiver actually
/// produced, content compared at the best available alignment - never a
/// length check, a `contains`, or an `ends_with`.
///
/// Task 5 measured why those three are not safe here: a cycle slip can
/// hand back the right byte count and zero framing errors while every
/// byte is wrong, and all three of those checks are blind to it - see
/// `measure_ber_scores_content_not_length_after_a_slip` below, which
/// reproduces exactly that shape of input. This walks a small window of
/// leading offsets into `recovered` (covering a training artefact or an
/// early slip that shifted the whole stream by a few bytes), scores each
/// offset on how many bytes actually match `payload`, and reports the
/// fraction wrong at whichever offset scores best. `payload` bytes beyond
/// whatever `recovered` covers at that offset count as wrong too - a
/// receiver that silently drops the tail of a message must not be scored
/// as though it delivered a shorter, perfect one.
pub fn measure_ber(payload: &[u8], recovered: &[u8]) -> f64 {
    if payload.is_empty() {
        return 0.0;
    }
    let max_offset = ALIGN_SEARCH.min(recovered.len());
    let mut best_wrong = payload.len();
    for offset in 0..=max_offset {
        let n = payload.len().min(recovered.len() - offset);
        let mismatched = payload[..n]
            .iter()
            .zip(&recovered[offset..offset + n])
            .filter(|(p, r)| p != r)
            .count();
        let wrong = (payload.len() - n) + mismatched;
        if wrong < best_wrong {
            best_wrong = wrong;
        }
    }
    best_wrong as f64 / payload.len() as f64
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nco::goertzel;
    use crate::rx::Rx;
    use crate::tx::Tx;
    use crate::{Config, Duplex, Role};

    /// A few dozen bytes, all-ASCII but varied - per this task's own
    /// timing note, content divergence shows up by byte index 2, so a
    /// short payload is enough everywhere except the cumulative-drift
    /// measurement, which uses its own long payload instead.
    const PAYLOAD: &[u8] = b"The quick brown fox jumps over the lazy dog. 0123456789";

    /// A Goertzel window sized to an integer number of cycles at `freq`
    /// against `fs`, at least `min_len` long - see resample.rs's own copy
    /// of this helper for why a non-integer cycle count understates the
    /// true magnitude. Duplicated here rather than shared because it is
    /// `resample::tests`-private.
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

    fn sine(freq: f64, rate: f64, n: usize) -> Vec<f32> {
        (0..n)
            .map(|i| sin(core::f64::consts::TAU * freq * i as f64 / rate) as f32)
            .collect()
    }

    // ------------------------------------------------------------------
    // clip
    // ------------------------------------------------------------------

    #[test]
    fn clip_limits_to_the_stated_level() {
        let mut s = vec![-2.0f32, -0.3, 0.0, 0.5, 2.0];
        clip(&mut s, 1.0);
        assert_eq!(s, vec![-1.0, -0.3, 0.0, 0.5, 1.0]);
    }

    #[test]
    fn clip_leaves_a_signal_inside_the_level_untouched() {
        let mut s: Vec<f32> = (0..100).map(|i| (i as f32 / 50.0) - 1.0).collect();
        let original = s.clone();
        clip(&mut s, 5.0); // level far above the signal's own range
        assert_eq!(
            s, original,
            "clip changed a signal that never exceeded level"
        );
    }

    // ------------------------------------------------------------------
    // harmonic_distortion
    // ------------------------------------------------------------------

    /// A 1070 Hz tone distorted this way must show real energy at its
    /// second harmonic, 2140 Hz, and not at its third, 3210 Hz - pins the
    /// specific harmonic this function claims to add. Mutation proof 3
    /// swaps `x * x` for `x * x * x` and shows this reverses.
    #[test]
    fn harmonic_distortion_adds_energy_at_the_second_harmonic() {
        let n = 4000;
        let mut s = sine(1070.0, DSP_RATE, n);
        harmonic_distortion(&mut s, 0.3);
        let s64: Vec<f64> = s.iter().map(|&v| v as f64).collect();
        let win2 = cycle_window(2140.0, DSP_RATE, 2000);
        let win3 = cycle_window(3210.0, DSP_RATE, 2000);
        let second = goertzel(&s64[n - win2..], 2140.0, DSP_RATE);
        let third = goertzel(&s64[n - win3..], 3210.0, DSP_RATE);
        assert!(
            second > 0.05,
            "no second-harmonic energy at 2140 Hz: {second}"
        );
        assert!(
            third < 0.01,
            "unexpected third-harmonic energy at 3210 Hz: {third}"
        );
    }

    #[test]
    fn harmonic_distortion_amount_zero_is_a_no_op() {
        let mut s = vec![0.1f32, -0.5, 0.9, -0.2];
        let original = s.clone();
        harmonic_distortion(&mut s, 0.0);
        assert_eq!(s, original);
    }

    // ------------------------------------------------------------------
    // band_limit
    // ------------------------------------------------------------------

    #[test]
    fn band_limit_passes_a_mid_band_tone() {
        let n = 6000;
        let mut s = sine(1700.0, DSP_RATE, n); // well inside 300-3400 Hz
        band_limit(&mut s, DSP_RATE);
        let s64: Vec<f64> = s.iter().map(|&v| v as f64).collect();
        let win = cycle_window(1700.0, DSP_RATE, 2000);
        let mag = goertzel(&s64[n - win..], 1700.0, DSP_RATE);
        assert!(mag > 0.9, "mid-band tone attenuated to {mag}");
    }

    #[test]
    fn band_limit_rejects_below_300hz() {
        let n = 6000;
        let mut s = sine(100.0, DSP_RATE, n);
        band_limit(&mut s, DSP_RATE);
        let s64: Vec<f64> = s.iter().map(|&v| v as f64).collect();
        let win = cycle_window(100.0, DSP_RATE, 2000);
        let mag = goertzel(&s64[n - win..], 100.0, DSP_RATE);
        assert!(
            mag < 0.1,
            "100 Hz tone survived the telephone passband at {mag}"
        );
    }

    #[test]
    fn band_limit_rejects_above_3400hz() {
        let n = 6000;
        let mut s = sine(3800.0, DSP_RATE, n);
        band_limit(&mut s, DSP_RATE);
        let s64: Vec<f64> = s.iter().map(|&v| v as f64).collect();
        let win = cycle_window(3800.0, DSP_RATE, 2000);
        let mag = goertzel(&s64[n - win..], 3800.0, DSP_RATE);
        assert!(
            mag < 0.1,
            "3800 Hz tone survived the telephone passband at {mag}"
        );
    }

    // ------------------------------------------------------------------
    // reverb
    // ------------------------------------------------------------------

    #[test]
    fn reverb_adds_a_delayed_scaled_copy() {
        let mut s = vec![1.0f32, 0.0, 0.0, 0.0, 0.0];
        reverb(&mut s, 2, 0.5);
        assert_eq!(s, vec![1.0, 0.0, 0.5, 0.0, 0.0]);
    }

    #[test]
    fn reverb_with_delay_beyond_the_buffer_is_a_no_op() {
        let mut s = vec![1.0f32, 2.0, 3.0];
        let original = s.clone();
        reverb(&mut s, 10, 0.9);
        assert_eq!(s, original);
    }

    // ------------------------------------------------------------------
    // duplex_leak
    // ------------------------------------------------------------------

    #[test]
    fn duplex_leak_mixes_near_under_far() {
        let far = vec![1.0f32, 1.0, 1.0];
        let near = vec![0.2f32, 0.4, 0.6];
        let out = duplex_leak(&far, &near, 0.5);
        assert_eq!(out, vec![1.1, 1.2, 1.3]);
    }

    #[test]
    fn duplex_leak_pads_a_shorter_near_with_silence() {
        let far = vec![1.0f32, 1.0, 1.0, 1.0];
        let near = vec![1.0f32];
        let out = duplex_leak(&far, &near, 1.0);
        assert_eq!(out, vec![2.0, 1.0, 1.0, 1.0]);
    }

    #[test]
    fn duplex_leak_truncates_a_longer_near() {
        let far = vec![0.0f32, 0.0];
        let near = vec![1.0f32, 1.0, 1.0, 1.0];
        let out = duplex_leak(&far, &near, 1.0);
        assert_eq!(out, vec![1.0, 1.0]);
        assert_eq!(out.len(), far.len());
    }

    // ------------------------------------------------------------------
    // clock_drift
    // ------------------------------------------------------------------

    #[test]
    fn clock_drift_zero_ppm_is_exact_passthrough() {
        let s = sine(1000.0, DSP_RATE, 2000);
        let out = clock_drift(&s, 0.0);
        assert_eq!(out, s, "zero drift changed a signal via the identity path");
    }

    #[test]
    fn clock_drift_changes_length_proportionally() {
        let s = vec![0.0f32; 8000];
        let out = clock_drift(&s, 20_000.0); // +2%
        let ratio = out.len() as f64 / s.len() as f64;
        assert!(
            (ratio - 1.02).abs() < 0.001,
            "length ratio {ratio}, expected close to 1.02"
        );
    }

    // ------------------------------------------------------------------
    // add_awgn
    // ------------------------------------------------------------------

    #[test]
    fn add_awgn_is_deterministic_for_a_given_seed() {
        let base = vec![0.5f32; 1000];
        let mut a = base.clone();
        let mut b = base.clone();
        add_awgn(&mut a, 10.0, 42);
        add_awgn(&mut b, 10.0, 42);
        assert_eq!(a, b, "same seed produced different noise");
    }

    #[test]
    fn add_awgn_different_seeds_differ() {
        let base = vec![0.5f32; 1000];
        let mut a = base.clone();
        let mut b = base.clone();
        add_awgn(&mut a, 10.0, 1);
        add_awgn(&mut b, 10.0, 2);
        assert_ne!(a, b, "different seeds produced identical noise");
    }

    #[test]
    fn add_awgn_leaves_silence_untouched() {
        let mut s = vec![0.0f32; 500];
        add_awgn(&mut s, 10.0, 99);
        assert!(
            s.iter().all(|&x| x == 0.0),
            "noise added to a zero-power signal"
        );
    }

    #[test]
    fn add_awgn_matches_its_stated_snr() {
        let n = 40_000;
        let mut s = sine(1000.0, DSP_RATE, n);
        for v in s.iter_mut() {
            *v *= 0.5;
        }
        let signal_power: f64 = s.iter().map(|&x| (x as f64) * (x as f64)).sum::<f64>() / n as f64;
        add_awgn(&mut s, 10.0, 123);
        let total_power: f64 = s.iter().map(|&x| (x as f64) * (x as f64)).sum::<f64>() / n as f64;
        let noise_power = total_power - signal_power;
        let measured_snr_db = 10.0 * (signal_power / noise_power).log10();
        assert!(
            (measured_snr_db - 10.0).abs() < 1.0,
            "measured SNR {measured_snr_db} dB, wanted close to 10 dB"
        );
    }

    // ------------------------------------------------------------------
    // measure_ber
    // ------------------------------------------------------------------

    #[test]
    fn measure_ber_zero_for_identical_streams() {
        assert_eq!(measure_ber(PAYLOAD, PAYLOAD), 0.0);
    }

    /// Exactly Task 5's shape: same byte count, but every byte wrong -
    /// the signature of a cycle slip that latched a wrong-but-self-
    /// consistent byte alignment. This is what mutation proof 1 targets:
    /// a length-only or count-only measure reads this as a perfect
    /// delivery, because both streams are 10 bytes long.
    #[test]
    fn measure_ber_scores_content_not_length_after_a_slip() {
        let payload = b"AAAAAAAAAA";
        let recovered = b"BBBBBBBBBB";
        assert_eq!(measure_ber(payload, recovered), 1.0);
    }

    #[test]
    fn measure_ber_penalises_a_dropped_tail() {
        let payload = b"0123456789";
        let recovered = b"01234";
        assert_eq!(measure_ber(payload, recovered), 0.5);
    }

    /// A leftover training byte (or similar bookkeeping artefact) ahead
    /// of the payload must not be scored as though every byte shifted.
    #[test]
    fn measure_ber_finds_a_small_leading_offset() {
        let payload = b"HELLO";
        let mut recovered = Vec::new();
        recovered.push(0x55u8);
        recovered.extend_from_slice(payload);
        assert_eq!(measure_ber(payload, &recovered), 0.0);
    }

    #[test]
    fn measure_ber_partial_mismatch_is_a_fraction() {
        let payload = b"AAAA";
        let recovered = b"AABA";
        assert_eq!(measure_ber(payload, recovered), 0.25);
    }

    #[test]
    fn measure_ber_empty_payload_is_zero() {
        assert_eq!(measure_ber(b"", b"anything"), 0.0);
    }

    /// Mutation 5 (own): an aggregate that counts which byte *values*
    /// appear, rather than comparing them position by position, is
    /// exactly as blind as the length check mutation 1 targets - a
    /// stream that delivers the right bytes in the wrong order has an
    /// identical value histogram to a correct delivery, so a value-count
    /// comparison reads it as perfect. This defect shape has already
    /// happened once in this project: Task 3's `Tx` popped its bit queue
    /// from the wrong end, reversing every byte, and passed all 15 tests
    /// until an oracle compared actual byte order rather than an
    /// aggregate over it. This pins the same property at the measurement
    /// layer itself.
    #[test]
    fn measure_ber_treats_reordered_bytes_as_wrong() {
        let payload = b"ABCDEFGH";
        let mut recovered = payload.to_vec();
        recovered.reverse();
        let ber = measure_ber(payload, &recovered);
        assert!(
            ber > 0.5,
            "reversed bytes measured ber {ber}, expected most positions to disagree"
        );
    }

    // ------------------------------------------------------------------
    // Calibration harness: a full Tx -> impairment -> Rx loopback, at
    // DSP_RATE so no device-rate resampling is in play beyond what an
    // impairment itself introduces (clock_drift). Used both by the
    // dedicated tests below and to produce the figures written into
    // docs/ber-calibration.md.
    // ------------------------------------------------------------------

    /// Renders `payload` to "air": settle, the acquisition preamble
    /// rx.rs's module doc requires (alternating symbols, then one
    /// character time of idle mark), then the payload. Impairments are
    /// applied to the returned buffer, exactly where a real acoustic
    /// channel would sit between two sound cards.
    fn air(payload: &[u8], role: Role) -> Vec<f32> {
        let c = Config {
            sample_rate: DSP_RATE as u32,
            role,
            duplex: Duplex::HalfPingPong,
        };
        let mut tx = Tx::new(c);
        let mut buf = Vec::new();
        fn emit(tx: &mut Tx, buf: &mut Vec<f32>, n: usize) {
            let mut b = vec![0.0f32; n];
            tx.read(&mut b);
            buf.extend_from_slice(&b);
        }
        emit(&mut tx, &mut buf, 1600); // settle, correlator window fills
        tx.write(&[0x55, 0x55]); // acquisition preamble: alternating symbols
        emit(&mut tx, &mut buf, 2 * 10 * DSP_RATE as usize / 300);
        emit(&mut tx, &mut buf, DSP_RATE as usize / 30); // one char time idle mark
        tx.write(payload);
        emit(
            &mut tx,
            &mut buf,
            payload.len() * 10 * DSP_RATE as usize / 300 + DSP_RATE as usize,
        );
        buf
    }

    /// Demodulates `samples` and returns everything after the two
    /// training bytes. Does not assume the training decoded as exactly
    /// `0x55, 0x55` - only that it took exactly two bytes, which rx.rs's
    /// own module doc establishes as reliable even when an individual
    /// training character comes back altered.
    fn demod(samples: &[f32], role: Role) -> Vec<u8> {
        let c = Config {
            sample_rate: DSP_RATE as u32,
            role,
            duplex: Duplex::HalfPingPong,
        };
        let mut rx = Rx::new(c);
        let mut out = Vec::new();
        let mut got = [0u8; 256];
        for chunk in samples.chunks(733) {
            rx.write(chunk);
            let n = rx.read(&mut got);
            out.extend_from_slice(&got[..n]);
        }
        if out.len() >= 2 {
            out.split_off(2)
        } else {
            Vec::new()
        }
    }

    #[test]
    fn clean_loopback_through_the_harness_has_zero_ber() {
        let clean = air(PAYLOAD, Role::Originate);
        let recovered = demod(&clean, Role::Originate);
        assert_eq!(measure_ber(PAYLOAD, &recovered), 0.0);
    }

    // ------------------------------------------------------------------
    // Calibration: the figures behind MAX_BYTE_ERROR_RATE and
    // docs/ber-calibration.md. Method: `air` renders a real Tx waveform
    // with the acquisition preamble rx.rs's module doc requires, the
    // impairment under test is applied to that buffer, `demod` runs it
    // through a real Rx, and `measure_ber` scores the result against the
    // known payload. Every number here was found by sweeping first (see
    // the task report for the full sweep output) and is pinned at a
    // point with real margin either side, not at the exact edge of a
    // cliff - the edges themselves are recorded in the report and in
    // docs/ber-calibration.md, not asserted on here, since a cliff edge
    // moves by construction and a test pinned exactly on one is a
    // regression detector for noise, not for behaviour.
    // ------------------------------------------------------------------

    /// Task 5 measured wideband AWGN as flat down to 6 dB SNR. Measured
    /// again here through this task's own harness: still flat at 3 dB,
    /// one dB lower than Task 5's own floor - and the first measurable
    /// movement is at 0 dB (1 of 55 bytes wrong), which is not part of
    /// this assertion because it is a genuinely different regime, not
    /// noise in this one. AWGN is not what gates this modem.
    #[test]
    fn awgn_is_flat_down_to_3db() {
        for db in [40.0, 25.0, 15.0, 10.0, 6.0, 3.0] {
            let mut s = air(PAYLOAD, Role::Originate);
            add_awgn(&mut s, db, 0xC0FFEE);
            let recovered = demod(&s, Role::Originate);
            assert_eq!(
                measure_ber(PAYLOAD, &recovered),
                0.0,
                "AWGN at {db} dB SNR was not flat"
            );
        }
    }

    /// A hard limiter barely matters until it clamps the whole signal
    /// down near the carrier detector's own cold-start sensitivity floor
    /// (~0.0075 amplitude - see `carrier.rs`'s `INITIAL_FLOOR`). Measured
    /// down to level 0.0055, comfortably above that floor with margin.
    #[test]
    fn clip_is_harmless_well_above_the_carrier_floor() {
        for level in [1.0f32, 0.5, 0.2, 0.05, 0.01, 0.0055] {
            let mut s = air(PAYLOAD, Role::Originate);
            clip(&mut s, level);
            let recovered = demod(&s, Role::Originate);
            assert_eq!(
                measure_ber(PAYLOAD, &recovered),
                0.0,
                "clip at level {level} was not harmless"
            );
        }
    }

    /// Below the carrier floor, clip does not corrupt bytes - it removes
    /// the signal entirely (measured: level 0.005 loses every byte, not
    /// just some of them), because the whole waveform is now quieter
    /// than what a fresh receiver treats as carrier at all.
    #[test]
    fn clip_below_the_carrier_floor_loses_the_signal_entirely() {
        let mut s = air(PAYLOAD, Role::Originate);
        clip(&mut s, 0.005);
        let recovered = demod(&s, Role::Originate);
        assert_eq!(measure_ber(PAYLOAD, &recovered), 1.0);
        assert!(
            recovered.is_empty(),
            "expected no carrier at all below the sensitivity floor"
        );
    }

    /// Bell 103 was designed to run over the phone network, so the
    /// telephone passband should not disturb it at all - both Originate
    /// tones (1270, 1070 Hz) sit well inside 300-3400 Hz.
    #[test]
    fn band_limit_does_not_disturb_a_clean_decode() {
        let mut s = air(PAYLOAD, Role::Originate);
        band_limit(&mut s, DSP_RATE);
        let recovered = demod(&s, Role::Originate);
        assert_eq!(measure_ber(PAYLOAD, &recovered), 0.0);
    }

    /// Cross-checks rx.rs's own documented Gardner pull-in range
    /// (+/-2.5%, from a completely different mechanism: two `Tx`/`Rx`
    /// pairs configured with differing device rates, rather than this
    /// function resampling one already-rendered buffer). 25,000 ppm is
    /// 2.5%; measured clean in both directions, with the asymmetry the
    /// sweep in the task report also shows (the positive direction's
    /// clean range in fact extends slightly further, to 2.8%).
    #[test]
    fn clock_drift_survives_the_documented_pull_in_range() {
        let mut payload = Vec::new();
        for _ in 0..100 {
            payload.extend_from_slice(b"The quick brown fox. ");
        }
        for ppm in [25_000.0, -25_000.0] {
            let clean = air(&payload, Role::Originate);
            let drifted = clock_drift(&clean, ppm);
            let recovered = demod(&drifted, Role::Originate);
            assert_eq!(
                measure_ber(&payload, &recovered),
                0.0,
                "clock drift {ppm} ppm was not absorbed"
            );
        }
    }

    /// Beyond the pull-in range the loop does not degrade gracefully -
    /// measured in the task report, it collapses hard (92-96% wrong)
    /// within half a percentage point of the boundary above. This pins
    /// deep inside the collapsed region, not at the fragile edge itself.
    #[test]
    fn clock_drift_beyond_pull_in_corrupts_most_of_the_content() {
        let mut payload = Vec::new();
        for _ in 0..100 {
            payload.extend_from_slice(b"The quick brown fox. ");
        }
        for ppm in [35_000.0, -30_000.0] {
            let clean = air(&payload, Role::Originate);
            let drifted = clock_drift(&clean, ppm);
            let recovered = demod(&drifted, Role::Originate);
            let ber = measure_ber(&payload, &recovered);
            assert!(
                ber > 0.5,
                "clock drift {ppm} ppm only reached ber {ber}, expected the loop to have lost lock"
            );
        }
    }

    /// A single early reflection at desk distance (2 ms - roughly the
    /// round-trip difference of a 30 cm reflection) is tolerated even at
    /// a fairly strong reflection coefficient.
    #[test]
    fn reverb_of_a_desk_distance_reflection_is_tolerated() {
        for gain in [0.3f32, 0.7] {
            let mut s = air(PAYLOAD, Role::Originate);
            reverb(&mut s, 16, gain);
            let recovered = demod(&s, Role::Originate);
            assert_eq!(
                measure_ber(PAYLOAD, &recovered),
                0.0,
                "reverb gain {gain} at 2 ms was not tolerated"
            );
        }
    }

    /// The same 2 ms reflection at gain 0.85 - a near-equal-amplitude
    /// echo, a strong and adversarial but not impossible reflection off
    /// a hard nearby surface - measured at 14.5% byte error rate. This is
    /// the dominant room effect the module doc names, and it is a real
    /// impairment, not a theoretical one.
    #[test]
    fn reverb_of_a_strong_close_reflection_corrupts_content() {
        let mut s = air(PAYLOAD, Role::Originate);
        reverb(&mut s, 16, 0.85);
        let recovered = demod(&s, Role::Originate);
        let ber = measure_ber(PAYLOAD, &recovered);
        assert!(
            ber > 0.1,
            "expected the strong close reflection to corrupt content, got ber {ber}"
        );
    }

    /// The flagship test. A payload continuously transmitted by
    /// Originate (so it carries genuine space-tone content, not just
    /// idle mark) is heavily overdriven and leaked into an Answer
    /// transmission at equal amplitude (`near_gain` 1.0 - loud, but not
    /// already overwhelming on its own: `moderate_originate_distortion_
    /// leaves_the_answer_direction_clean` below shows the same leak with
    /// less distortion staying clean). Measured in the task report: this
    /// specific combination is where corruption first appears as
    /// `amount` rises (clean at 1.5, 50% wrong at 1.6) - proving the
    /// mechanism `harmonic_distortion`'s doc claims, not just asserting
    /// it. Mutation proof 3 swaps the second harmonic for the third at
    /// this exact scenario and the corruption disappears.
    #[test]
    fn loud_originate_harmonic_corrupts_the_answer_direction() {
        let answer_payload = b"NO CARRIER 0123456789 ABCDEFGHIJKLMNOPQRSTUVWXYZ";
        let mut distorted = air(PAYLOAD, Role::Originate);
        harmonic_distortion(&mut distorted, 1.6);
        let answer_air = air(answer_payload, Role::Answer);
        let mixed = duplex_leak(&answer_air, &distorted, 1.0);
        let recovered = demod(&mixed, Role::Answer);
        let ber = measure_ber(answer_payload, &recovered);
        assert!(
            ber > 0.3,
            "expected the second-harmonic leak to corrupt the Answer direction, got ber {ber}"
        );
    }

    /// The control for the test above: the same leak, at the same
    /// amplitude, with distortion mild enough that the second harmonic's
    /// energy at 2140 Hz has not yet grown large enough to matter. Rules
    /// out "any leak at this gain always breaks it regardless of
    /// distortion", which would make the test above meaningless.
    #[test]
    fn moderate_originate_distortion_leaves_the_answer_direction_clean() {
        let answer_payload = b"NO CARRIER 0123456789 ABCDEFGHIJKLMNOPQRSTUVWXYZ";
        let mut distorted = air(PAYLOAD, Role::Originate);
        harmonic_distortion(&mut distorted, 1.0);
        let answer_air = air(answer_payload, Role::Answer);
        let mixed = duplex_leak(&answer_air, &distorted, 1.0);
        let recovered = demod(&mixed, Role::Answer);
        assert_eq!(measure_ber(answer_payload, &recovered), 0.0);
    }

    /// A plausible physical chain at levels well short of any individual
    /// cliff measured above: source overdrive, one desk-distance
    /// reflection, ambient noise at 25 dB SNR (Task 17's own acceptance
    /// figure), then a moderately hot receiving preamp. Measured clean -
    /// this is the reference point MAX_BYTE_ERROR_RATE is set against.
    #[test]
    fn a_realistic_combined_room_scenario_stays_within_the_gate() {
        let mut s = air(PAYLOAD, Role::Originate);
        harmonic_distortion(&mut s, 0.3);
        reverb(&mut s, 16, 0.3);
        add_awgn(&mut s, 25.0, 0xF00D);
        clip(&mut s, 0.7);
        let recovered = demod(&s, Role::Originate);
        let ber = measure_ber(PAYLOAD, &recovered);
        assert!(
            ber <= MAX_BYTE_ERROR_RATE,
            "realistic combined room scenario measured ber {ber}, above the gate"
        );
    }
}
