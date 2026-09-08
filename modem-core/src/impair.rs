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
/// 0.01 (1%) sits in the empty band between every configuration this
/// task measured working on a single, fixed, reproducible scenario
/// (at most 0.0005, one byte in 2,100) and every one it measured
/// failing (at least 0.018), and it coincides with Task 19's real-world
/// desk-test figure ("under 1% of characters corrupted"). Task 19's
/// figure corroborates this constant; it does not carry it - Task 19
/// has not run yet, and a target that has not been checked against
/// reality cannot distinguish "the gate was wrong" from "the modem got
/// worse" on its own. The measured band can, today, and does.
///
/// Fix round 1 tried to anchor this constant to one specific `clock_
/// drift` measurement (+30,000 ppm on a periodic payload, measuring
/// exactly 0.01). That did not survive review: the figure was a
/// property of the payload's 21-byte period, not the modem, and moved
/// by 67x once the period was removed.
///
/// Fix round 2 found the replacement anchor had the same disease one
/// level up: -28,000 ppm on a *single* shuffled (non-periodic) payload
/// measured 0.0024, but the identical 2,100-byte multiset under nine
/// other arbitrary shuffles spans 0.0376 to 0.56 at that same drift
/// value - a 234x spread, and only one draw in ten actually clears the
/// gate. Fix round 2's own fix - sweep several seeds at +/-25,000 ppm
/// and require the *maximum* across all of them to clear the gate -
/// was itself wrong in the same way one level up again: it turned an
/// existence claim into a universal one. A wider, independent sweep of
/// 36 seeds found 4 collapse to `ber = 1.0` at -25,000 ppm (genuine
/// collapses, confirmed with an unbounded alignment search, not a
/// measurement artefact) - roughly an 11% rate, meaning fix round 2's
/// own all-clean 8-seed draw had about a 40% chance of happening by
/// luck.
///
/// Fix round 3: a bracket only needs to show *one* real, reproducible,
/// non-vacuous scenario on each side of the gate - it does not need to
/// hold for every possible payload, and asking it to invites exactly
/// the lottery above. `clock_drift_survives_the_documented_pull_in_
/// range` (seed 2, fixed) is the lower bracket; `a_scenario_above_the_
/// gate_is_correctly_rejected` (AWGN at 0 dB on `long_payload`, stable
/// across seeds because 2,100 bytes is enough for a noise process to
/// average out, unlike the 55-byte `PAYLOAD` fix round 2 used there) is
/// an upper one.
///
/// Fix round 4: that single upper bracket (0.82+) left an unpinned band
/// between the gate and its own value - 0.05 and 0.1 are both real,
/// 5x-to-10x loosenings, and both passed every workspace test
/// unnoticed. Closed with two more brackets, each covering the part of
/// the range the others miss: `reverb_of_a_strong_close_reflection_
/// corrupts_content` (0.145455, fully deterministic) catches a gate
/// raised to 0.5 but not to 0.1; `a_scenario_just_above_the_gate_is_
/// correctly_rejected` (AWGN at 0 dB on the 55-byte `PAYLOAD`, the exact
/// scenario fix round 3 removed from the near side, at a fixed seed)
/// catches anything above 0.018182. None of the three claims to be
/// typical or universal; each is a concrete, disclosed, reproducible
/// fact, which is what a bracket actually needs to be. See `docs/ber-
/// calibration.md` for the full picture, including the honest one: no
/// clock-offset tolerance figure at or above 2.5% is safe to quote as
/// payload-independent - every collapse measured across all rounds sits
/// on the negative-drift side, but a tolerance figure is quoted
/// symmetrically, so +/-2.5% remains the boundary that can be quoted.
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
/// pure tone of amplitude `A` at frequency f produces energy at 2f this
/// way (`A^2 sin^2(wt) = A^2/2 - (A^2/2) cos(2wt)`), which is exactly the
/// mechanism a mildly nonlinear, asymmetric amplifier stage adds - the
/// even-order term in its Taylor expansion around the operating point.
/// The second harmonic's own amplitude is `amount * A^2 / 2` - at `A = 1`
/// that is `amount / 2`, so `amount = 1.6` is an 80% second-harmonic-to-
/// fundamental ratio.
///
/// That expansion also carries an equal-sized DC term (`A^2/2`, scaled by
/// `amount`), which this function does add to the signal. No acoustic
/// path passes DC, so it plays no part in anything this crate's
/// correlator-based tests measure, but it is a real part of this
/// function's output and worth naming rather than leaving implicit.
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

/// How far [`measure_ber`] searches for the best alignment between
/// `payload` and `recovered`, in bytes, in either direction.
///
/// Fix round 1: 8, one-directional, was not enough. Measured directly at
/// -30,000 ppm clock drift: the receiver mangles the opening ~15
/// characters of a lost-then-reacquired carrier before it locks back on
/// and delivers the rest correctly, and the true alignment sits at
/// offset 17 - outside the old window even in the one direction it
/// searched. 32, both directions, covers that with margin: a training
/// artefact or a brief reacquisition in either direction, without
/// searching so far that it starts finding accidental matches in a
/// payload's own structure rather than a genuine alignment.
const ALIGN_SEARCH: isize = 32;

/// Byte error rate between a known `payload` and what a receiver actually
/// produced, content compared at the best available alignment - never a
/// length check, a `contains`, or an `ends_with`.
///
/// Task 5 measured why those three are not safe here: a cycle slip can
/// hand back the right byte count and zero framing errors while every
/// byte is wrong, and all three of those checks are blind to it - see
/// `measure_ber_scores_content_not_length_after_a_slip` below, which
/// reproduces exactly that shape of input. This searches a window of
/// offsets in both directions - `recovered` may carry leading content
/// that is not part of the payload (a training artefact, or a mangled
/// reacquisition after a lost carrier - see [`ALIGN_SEARCH`]), or it may
/// be missing leading payload content outright (a receiver that ate the
/// very first byte) - scores each offset on how many bytes actually
/// match `payload`, and reports the fraction wrong at whichever offset
/// scores best. Bytes of `payload` that a given offset skips or leaves
/// uncovered count as wrong - a receiver that silently drops part of a
/// message must not be scored as though it delivered a shorter, perfect
/// one.
pub fn measure_ber(payload: &[u8], recovered: &[u8]) -> f64 {
    if payload.is_empty() {
        return 0.0;
    }
    let mut best_wrong = payload.len();
    for offset in -ALIGN_SEARCH..=ALIGN_SEARCH {
        // offset >= 0: recovered has `offset` extra leading bytes ahead
        // of the payload. offset < 0: recovered is missing that many of
        // the payload's own leading bytes.
        let (p_start, r_start) = if offset >= 0 {
            (0usize, offset as usize)
        } else {
            ((-offset) as usize, 0usize)
        };
        if p_start > payload.len() || r_start > recovered.len() {
            continue;
        }
        let n = (payload.len() - p_start).min(recovered.len() - r_start);
        let mismatched = payload[p_start..p_start + n]
            .iter()
            .zip(&recovered[r_start..r_start + n])
            .filter(|(p, r)| p != r)
            .count();
        // Every payload byte this offset does not land a comparison on -
        // skipped at the head (p_start) or left uncovered at the tail -
        // counts as wrong, on top of any actual mismatch within the
        // compared span.
        let wrong = p_start + (payload.len() - p_start - n) + mismatched;
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

    /// A long, non-periodic payload for `clock_drift`'s cumulative-drift
    /// measurements.
    ///
    /// Fix round 1 finding (Critical 1): `"The quick brown fox. "` x100
    /// has a 21-byte period, and `clock_drift`'s resampling group delay
    /// interacts with that period in a way that makes the measured byte
    /// error rate a property of the *payload's* structure, not the
    /// modem's. Independently reproduced: the same 2,100 bytes,
    /// deterministically shuffled to keep the byte multiset but remove
    /// the period, moved the measured rate at +30,000 ppm by a factor of
    /// 67 (0.01 -> 0.667) and reversed which drift direction looked worse.
    ///
    /// Fix round 2 correction: an earlier version of this comment blamed
    /// a rejected fully-random-bytes attempt on top-bit parity (uniform
    /// bytes have their top bit set half the time, versus every ASCII
    /// byte here having it clear). That diagnosis was wrong - review
    /// measured a control payload with the top bit clear on *every*
    /// byte still spanning 0.31-0.99 at +/-28,000 ppm, exactly as bad as
    /// unconstrained random bytes. The real mechanism is *ordering*, not
    /// bit parity or transition density: a Gardner loop only corrects
    /// its timing at an actual symbol transition, so a long run of the
    /// same symbol lets accumulated clock-offset phase error grow
    /// unchecked, and *where* the longest such runs land relative to
    /// that accumulated error is a property of the specific byte
    /// sequence, not its statistics in aggregate. This is the same
    /// phase-lottery mechanism `rx.rs`'s own module doc already
    /// documents for lead-in offsets, not a new one - see
    /// `docs/ber-calibration.md` for the measured spread this produces
    /// and why no single clock_drift figure near the pull-in boundary is
    /// safe to quote as though it were payload-independent.
    fn long_payload() -> Vec<u8> {
        shuffled_sentence(0x1234_5678_9ABC_DEF0)
    }

    /// The same 2,100-byte multiset as `long_payload`, shuffled by a
    /// caller-chosen seed instead of the one fixed canonical shuffle.
    /// Exists because fix round 2's own finding is that *one* shuffle is
    /// not enough evidence about anything near the pull-in boundary -
    /// several of this crate's own tests need to sweep multiple
    /// independent shuffles of the identical content to show a result
    /// is a property of the drift, not of which shuffle happened to be
    /// picked.
    fn shuffled_sentence(seed: u64) -> Vec<u8> {
        let mut bytes = Vec::new();
        for _ in 0..100 {
            bytes.extend_from_slice(b"The quick brown fox. ");
        }
        // Deterministic Fisher-Yates shuffle (this crate's own xorshift,
        // not rand): keeps the exact byte multiset and hence identical
        // per-byte bit statistics, while destroying the original 21-byte
        // period. This is not a complete fix on its own, only a
        // necessary one - see `long_payload`'s own doc and
        // `docs/ber-calibration.md` for what shuffling does and does not
        // control for.
        let mut state = seed_state(seed);
        for i in (1..bytes.len()).rev() {
            let j = (xorshift_next(&mut state) as usize) % (i + 1);
            bytes.swap(i, j);
        }
        bytes
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

    /// Demodulates `samples` and returns everything the receiver actually
    /// decoded, unmodified - training bytes and all.
    ///
    /// Fix round 1: this used to assume the first two bytes were always
    /// the training characters and strip them unconditionally. True on a
    /// clean channel; false under impairment - measured directly at
    /// -30,000 ppm clock drift, the raw stream opens with over a dozen
    /// mangled bytes from a lost-then-reacquired carrier, not two. The
    /// harness was assuming the very thing it exists to measure. Locating
    /// the payload within the raw stream is `measure_ber`'s job now (its
    /// bidirectional alignment search), not this function's - which also
    /// means this same harness shape will work unmodified on a stream
    /// that carries no training convention of this crate's own at all,
    /// which is exactly what Task 9's minimodem cross-validation decodes.
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
        out
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
    // point with real margin either side, not at a fragile boundary -
    // the boundaries themselves are recorded in the report and in
    // docs/ber-calibration.md, not asserted on here, since a boundary
    // moves by construction and a test pinned exactly on one is a
    // regression detector for noise, not for behaviour. Fix round 2:
    // several of the boundaries near the Gardner pull-in range turned
    // out to move by payload structure as much as by drift itself - see
    // `clock_drift_near_the_boundary_is_payload_sensitive` below and
    // `MAX_BYTE_ERROR_RATE`'s own doc comment.
    //
    // Fix round 1 correction: four tests here used to assert only that
    // the recovered stream still matched the payload, which passes just
    // as well if the impairment call silently did nothing at all - an
    // aggregate (byte error rate) invariant under a real defect (a no-op
    // impairment), the exact pattern the brief warns about. Each now
    // also asserts the impaired buffer actually differs from the clean
    // one before checking what that change cost.
    // ------------------------------------------------------------------

    /// Task 5 measured wideband AWGN as flat down to 6 dB SNR across a 4x
    /// range of loop gain. Reconfirmed here at the harness level: 40, 25,
    /// 15 and 10 dB each with one reproducible seed (comfortably above
    /// the floor, one seed is enough evidence), and 6 dB itself - the
    /// boundary that actually needs more than one seed to trust - across
    /// 8 independent seeds, all flat.
    ///
    /// Fix round 2 published a 3 dB claim (one dB past Task 5's own
    /// floor) based on a single seed; a wider sample of 16 seeds found
    /// one exception (0.0182). Fix round 3 does not re-assert that
    /// exception here - a test pinning "the modem is wrong in this
    /// specific way" would have to be edited if a future change fixed
    /// it, which is backwards for a regression suite. The exception is
    /// recorded in `docs/ber-calibration.md` instead. 6 dB, Task 5's own
    /// figure, is the only floor this test stands behind.
    #[test]
    fn awgn_is_flat_down_to_6db() {
        for db in [40.0, 25.0, 15.0, 10.0] {
            let mut s = air(PAYLOAD, Role::Originate);
            add_awgn(&mut s, db, 0xC0FFEE);
            let recovered = demod(&s, Role::Originate);
            assert_eq!(
                measure_ber(PAYLOAD, &recovered),
                0.0,
                "AWGN at {db} dB SNR was not flat"
            );
        }
        for seed in 1u64..=8 {
            let mut s = air(PAYLOAD, Role::Originate);
            add_awgn(&mut s, 6.0, seed);
            let recovered = demod(&s, Role::Originate);
            assert_eq!(
                measure_ber(PAYLOAD, &recovered),
                0.0,
                "AWGN at 6 dB SNR with seed {seed} was not flat"
            );
        }
    }

    /// The gate needs a real scenario it correctly rejects, or it is a
    /// free constant (fix round 1, Critical 3: setting MAX_BYTE_ERROR_RATE
    /// to 0.5, or to 0.0, passed every test in the first submission,
    /// because the only test referencing it checked a measured 0.0
    /// against it, which holds for any non-negative gate). This is the
    /// far-side upper bracket: AWGN at 0 dB SNR fails if
    /// MAX_BYTE_ERROR_RATE is ever raised anywhere near this scenario's
    /// own value or beyond.
    ///
    /// Fix round 3 correction: fix round 2 ran this on the 55-byte
    /// `PAYLOAD`, where six seeds checked spanned 0.018 to 0.836 - the
    /// instability was a small-sample artefact, not a property of AWGN
    /// at 0 dB. 55 bytes is too few for a noise process to average out
    /// over; the same scenario on the 2,100-byte `long_payload` is
    /// stable across seeds - the minimum across 8 checked is 0.82, over
    /// 80x the gate, with no draw anywhere near it. Same mechanism, same
    /// narrative, instability explained by payload length rather than
    /// routed around by picking a different mechanism.
    ///
    /// Fix round 4 note: this test's own 0.82+ margin is too generous to
    /// notice a gate loosened to anything below that - see
    /// `a_scenario_just_above_the_gate_is_correctly_rejected` below for
    /// the near-side bracket that closes the resulting gap.
    #[test]
    fn a_scenario_above_the_gate_is_correctly_rejected() {
        let payload = long_payload();
        let mut s = air(&payload, Role::Originate);
        add_awgn(&mut s, 0.0, 0xC0FFEE);
        let recovered = demod(&s, Role::Originate);
        let ber = measure_ber(&payload, &recovered);
        assert!(
            ber > MAX_BYTE_ERROR_RATE,
            "AWGN at 0 dB measured ber {ber}, expected it to sit above the gate"
        );
    }

    /// The near-side upper bracket - fix round 4 addition. The far-side
    /// bracket above (minimum 0.82 across seeds on `long_payload`) and
    /// the reverb scenario further down (0.145455, fully deterministic)
    /// left an unpinned band: a gate loosened to 0.05 or 0.1 - a 5x or
    /// 10x relaxation, not a small one - passed all 100 workspace tests
    /// unnoticed, since 0.1 < 0.145455 and 0.05 and 0.1 are both well
    /// under the far-side bracket's 0.82.
    ///
    /// This closes it with the scenario fix round 3 removed from the
    /// near side: AWGN at 0 dB on the original 55-byte `PAYLOAD`, at the
    /// fixed seed `0xC0FFEE`, deterministically measures 0.018182 (1 of
    /// 55 bytes wrong) every time. Fix round 3 moved this off the near
    /// side because six *different* seeds on this payload spanned 0.018
    /// to 0.836 - a real finding, but about the general claim "AWGN at
    /// 0 dB on a short payload measures around X", not about this one
    /// fixed, reproducible measurement. Round 3's own thesis - a bracket
    /// only needs to be a true existence claim, not a universal one -
    /// applies here exactly as it does to the seedless reverb scenario:
    /// a fixed seed is exactly as reproducible as no seed at all. This
    /// brings the unpinned band down from 14.5x the gate to 1.8x, for
    /// about 0.3 s of test time.
    #[test]
    fn a_scenario_just_above_the_gate_is_correctly_rejected() {
        let mut s = air(PAYLOAD, Role::Originate);
        add_awgn(&mut s, 0.0, 0xC0FFEE);
        let recovered = demod(&s, Role::Originate);
        let ber = measure_ber(PAYLOAD, &recovered);
        assert!(
            ber > MAX_BYTE_ERROR_RATE,
            "AWGN at 0 dB on PAYLOAD measured ber {ber}, expected it to sit just above the gate"
        );
    }

    /// A hard limiter barely matters until it clamps the whole signal
    /// down near the carrier detector's own cold-start sensitivity floor
    /// (~0.0075 amplitude - see `carrier.rs`'s `INITIAL_FLOOR`). Measured
    /// down to level 0.0055, comfortably above that floor with margin.
    /// Level 1.0 is excluded deliberately: it sits at this signal's own
    /// peak amplitude, so clipping to it is legitimately a no-op and
    /// cannot carry the "the impairment actually did something"
    /// assertion below - `clip_leaves_a_signal_inside_the_level_
    /// untouched` already covers that case directly.
    #[test]
    fn clip_is_harmless_well_above_the_carrier_floor() {
        for level in [0.5f32, 0.2, 0.05, 0.01, 0.0055] {
            let clean = air(PAYLOAD, Role::Originate);
            let mut s = clean.clone();
            clip(&mut s, level);
            assert_ne!(
                s, clean,
                "clip at level {level} did not change the signal - the impairment may be a no-op"
            );
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
        let clean = air(PAYLOAD, Role::Originate);
        let mut s = clean.clone();
        band_limit(&mut s, DSP_RATE);
        assert_ne!(
            s, clean,
            "band_limit did not change the signal - the impairment may be a no-op"
        );
        let recovered = demod(&s, Role::Originate);
        assert_eq!(measure_ber(PAYLOAD, &recovered), 0.0);
    }

    /// A true existence claim, not a universal one - fix round 3
    /// correction. Fix round 2 swept 8 shuffle seeds here and asserted
    /// every one stayed inside the gate at +/-25,000 ppm, reframing
    /// this as a claim about the *boundary*. Independent review swept
    /// 36 further seeds and found 4 collapse to `ber = 1.0` at
    /// -25,000 ppm - a genuine result (confirmed with an unbounded
    /// alignment search: those seeds score 1.0 at offset zero, nothing
    /// recovered anywhere, not a search-window artefact) at roughly an
    /// 11% rate per seed. Round 2's own 8-seed draw had about a 40%
    /// chance of coming up all-clean. Sweeping was the wrong fix - it
    /// converted an existence claim (this specific, fixed, reproducible
    /// scenario measures a real rate inside the gate) into a universal
    /// one (no payload can fail here), which the wider sample falsifies.
    /// See `MAX_BYTE_ERROR_RATE`'s own doc comment and `docs/ber-
    /// calibration.md` for the corrected, payload-dependent picture -
    /// no clock-offset tolerance figure at or above 2.5% is safe to
    /// quote in either direction.
    ///
    /// What this test claims is narrower and still true: seed 2's
    /// shuffle measures a real, small, non-zero rate comfortably inside
    /// the gate at both edges of the documented range, and that rate is
    /// load-bearing - mutating `GARDNER_GAIN` from 0.15 to 0.03 fails
    /// this assertion outright (see the task report's mutation proofs).
    /// This is also `MAX_BYTE_ERROR_RATE`'s lower bracket, replacing fix
    /// round 2's 8-seed sweep and fix round 1's `a_scenario_below_the_
    /// gate_is_correctly_accepted` (both retired).
    #[test]
    fn clock_drift_survives_the_documented_pull_in_range() {
        let payload = shuffled_sentence(2);
        for ppm in [25_000.0, -25_000.0] {
            let clean = air(&payload, Role::Originate);
            let drifted = clock_drift(&clean, ppm);
            let recovered = demod(&drifted, Role::Originate);
            let ber = measure_ber(&payload, &recovered);
            assert!(
                ber > 0.0 && ber <= MAX_BYTE_ERROR_RATE,
                "seed 2 at {ppm} ppm measured ber {ber}, expected a real but small rate inside the gate"
            );
        }
    }

    /// Confirms the payload-sensitivity finding itself as a running fact,
    /// not only a claim in the docs: two arbitrary shuffles of the
    /// identical 2,100-byte multiset, at the identical -28,000 ppm drift,
    /// land on opposite sides of the gate. Neither "gradual" nor "cliff"
    /// describes this - both words imply a single number is the right
    /// way to characterise degradation near the boundary, and the point
    /// is that no single number is. See `MAX_BYTE_ERROR_RATE`'s own doc
    /// comment and `docs/ber-calibration.md` for the full ten-shuffle
    /// spread this pins two points from (0.0024 to 0.5619, only one draw
    /// in ten inside the gate).
    #[test]
    fn clock_drift_near_the_boundary_is_payload_sensitive() {
        let inside = shuffled_sentence(0x1234_5678_9ABC_DEF0);
        let outside = shuffled_sentence(0x1111_1111_1111_1111);

        let clean = air(&inside, Role::Originate);
        let drifted = clock_drift(&clean, -28_000.0);
        let recovered = demod(&drifted, Role::Originate);
        let ber = measure_ber(&inside, &recovered);
        assert!(
            ber <= MAX_BYTE_ERROR_RATE,
            "expected this shuffle to sit inside the gate at -28,000 ppm, measured {ber}"
        );

        let clean = air(&outside, Role::Originate);
        let drifted = clock_drift(&clean, -28_000.0);
        let recovered = demod(&drifted, Role::Originate);
        let ber = measure_ber(&outside, &recovered);
        assert!(
            ber > 0.5,
            "expected this shuffle to sit well outside the gate at -28,000 ppm, measured {ber}"
        );
    }

    /// Well beyond the pull-in range, corruption is severe regardless of
    /// payload shuffle - unlike the boundary itself, this region is
    /// stable: 10 independent shuffles all measure above 0.89 at both
    /// +35,000 and -35,000 ppm.
    ///
    /// Fix round 1 correction (Critical 2): the first submission's
    /// equivalent test asserted "the loop has lost lock" at -30,000 ppm,
    /// which was false - the receiver mangles the opening of a
    /// lost-then-reacquired carrier, then locks back on and delivers the
    /// remainder correctly. The reported 92-96% figure was an artefact
    /// of `ALIGN_SEARCH` being too narrow and one-directional to find
    /// that correctly reacquired remainder at all, so it scored the
    /// whole message wrong. -30,000 ppm is no longer used here for
    /// exactly that reason - it sits in the payload-sensitive region
    /// this file's other tests now cover directly, not the stable
    /// collapsed one this test pins.
    ///
    /// Residual, not fixed this round: `ALIGN_SEARCH = 32` is itself
    /// still too narrow for some points out here - the true alignment
    /// at -35,000 ppm on `long_payload` sits at offset -118, which this
    /// module's search cannot reach, so the figure below is itself an
    /// over-report (measured 0.9571 against a wider-search true value of
    /// 0.9333). Left as is: over-reporting corruption fails safe for a
    /// release gate, unlike Critical 2's under-reported collapse did.
    /// Flagged here because Task 9's minimodem harness will build on
    /// this same `measure_ber`, and should not assume +/-32 bytes is
    /// enough for every stream it decodes.
    #[test]
    fn clock_drift_well_beyond_pull_in_corrupts_most_of_the_content() {
        let payload = long_payload();
        for ppm in [35_000.0, -35_000.0] {
            let clean = air(&payload, Role::Originate);
            let drifted = clock_drift(&clean, ppm);
            let recovered = demod(&drifted, Role::Originate);
            let ber = measure_ber(&payload, &recovered);
            assert!(
                ber > 0.5,
                "clock drift {ppm} ppm only reached ber {ber}, expected substantial corruption"
            );
        }
    }

    /// A single early reflection at desk distance (2 ms - roughly the
    /// round-trip difference of a 30 cm reflection) is tolerated even at
    /// a fairly strong reflection coefficient.
    #[test]
    fn reverb_of_a_desk_distance_reflection_is_tolerated() {
        for gain in [0.3f32, 0.7] {
            let clean = air(PAYLOAD, Role::Originate);
            let mut s = clean.clone();
            reverb(&mut s, 16, gain);
            assert_ne!(
                s, clean,
                "reverb gain {gain} did not change the signal - the impairment may be a no-op"
            );
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
    ///
    /// Fix round 3 addition: this scenario is fully deterministic (no
    /// seed anywhere in `reverb`), reproducibly measures 0.145455, and
    /// that value sits usefully between `MAX_BYTE_ERROR_RATE` and the
    /// far-side bracket's own 0.82+. Asserting against the gate directly
    /// here, rather than a separately-chosen 0.1, means this fails if
    /// the gate is ever raised as far as 0.145455, while still
    /// demonstrating the real corruption this test exists to show at
    /// the actual gate value. This is the mid-side bracket: it catches
    /// a gate raised to 0.5, but not one raised to 0.05 or 0.1, since
    /// both sit below 0.145455 - `a_scenario_just_above_the_gate_is_
    /// correctly_rejected` (fix round 4) is the one that catches those.
    #[test]
    fn reverb_of_a_strong_close_reflection_corrupts_content() {
        let mut s = air(PAYLOAD, Role::Originate);
        reverb(&mut s, 16, 0.85);
        let recovered = demod(&s, Role::Originate);
        let ber = measure_ber(PAYLOAD, &recovered);
        assert!(
            ber > MAX_BYTE_ERROR_RATE,
            "expected the strong close reflection to corrupt content beyond the gate, got ber {ber}"
        );
    }

    /// Fix round 1 finding (Important 3): a leak without any harmonic
    /// distortion at all still corrupts the Answer direction once it is
    /// loud enough on its own. The first submission's report claimed
    /// correlator selectivity held for an undistorted leak up to
    /// `near_gain` 20 "without breaking anything", which was never
    /// actually true past 2.0 - gain 1.0 and 2.0 measure clean, but gain
    /// 3.0 measures 0.81. Sheer loudness, no harmonic content required,
    /// is enough by itself.
    #[test]
    fn undistorted_leak_at_high_gain_alone_corrupts_the_answer_direction() {
        let answer_payload = b"NO CARRIER 0123456789 ABCDEFGHIJKLMNOPQRSTUVWXYZ";
        let originate_air = air(PAYLOAD, Role::Originate);
        let answer_air = air(answer_payload, Role::Answer);
        let mixed = duplex_leak(&answer_air, &originate_air, 3.0);
        let recovered = demod(&mixed, Role::Answer);
        let ber = measure_ber(answer_payload, &recovered);
        assert!(
            ber > MAX_BYTE_ERROR_RATE,
            "expected gain 3.0 with no distortion to corrupt the Answer direction, got ber {ber}"
        );
    }

    /// The flagship test. A payload continuously transmitted by
    /// Originate (so it carries genuine space-tone content, not just
    /// idle mark) is distorted at `amount = 1.6` - an 80%
    /// second-harmonic-to-fundamental ratio (the second harmonic's
    /// amplitude is `amount * A^2 / 2` for an input of amplitude `A`; at
    /// `A = 1` that is `amount / 2`), alongside an equal DC offset that
    /// no acoustic path actually passes and so plays no part here - and
    /// leaked into an Answer transmission at equal amplitude (`near_gain`
    /// 1.0: loud, but not already overwhelming on its own at this
    /// distortion level - `moderate_originate_distortion_leaves_the_
    /// answer_direction_clean` below and `undistorted_leak_at_high_gain_
    /// alone_corrupts_the_answer_direction` above are the two controls
    /// that isolate this specific mechanism from "any loud enough leak
    /// breaks it regardless"). Measured: clean at amount 1.5 (75%
    /// ratio), 50% wrong at 1.6 - proving the mechanism `harmonic_
    /// distortion`'s doc claims, not just asserting it. Mutation proof 3
    /// swaps the second harmonic for the third at this exact scenario
    /// and the corruption disappears.
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
    /// amplitude, with distortion mild enough (amount 1.0, a 50% second-
    /// harmonic-to-fundamental ratio) that the second harmonic's energy
    /// at 2140 Hz has not yet grown large enough to matter. Rules out
    /// "any leak at this gain always breaks it regardless of
    /// distortion", which would make the test above meaningless -
    /// together with `undistorted_leak_at_high_gain_alone_corrupts_the_
    /// answer_direction`, this pins the actual shape of the mechanism:
    /// safe at gain 1.0 regardless of moderate distortion, safe at any
    /// distortion level tested up to 1.5 regardless of gain 1.0, and
    /// broken only once *both* are pushed far enough.
    #[test]
    fn moderate_originate_distortion_leaves_the_answer_direction_clean() {
        let answer_payload = b"NO CARRIER 0123456789 ABCDEFGHIJKLMNOPQRSTUVWXYZ";
        let mut distorted = air(PAYLOAD, Role::Originate);
        harmonic_distortion(&mut distorted, 1.0);
        let answer_air = air(answer_payload, Role::Answer);
        let mixed = duplex_leak(&answer_air, &distorted, 1.0);
        assert_ne!(
            mixed, answer_air,
            "the leak did not change the answer-direction signal - the leak may be absent, not merely harmless"
        );
        let recovered = demod(&mixed, Role::Answer);
        assert_eq!(measure_ber(answer_payload, &recovered), 0.0);
    }

    /// A plausible physical chain at levels well short of any individual
    /// collapse measured above: source overdrive, one desk-distance
    /// reflection, ambient noise at 25 dB SNR (Task 17's own acceptance
    /// figure), then a moderately hot receiving preamp. Measured clean -
    /// this is the reference point `MAX_BYTE_ERROR_RATE` is checked
    /// against in this suite. The constant's own doc comment carries its
    /// full justification (the empty band between every measured working
    /// and failing configuration, corroborated by Task 19's real-world
    /// figure); `a_scenario_above_the_gate_is_correctly_rejected` and
    /// `clock_drift_survives_the_documented_pull_in_range` are its two
    /// bracketing tests.
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
