//! Turns a block of samples into a magnitude spectrum for the waterfall.
//!
//! Two Bell 103 pairs, 1070/1270 and 2025/2225, sit 200 Hz apart. A bin
//! wider than that would blur the two tones of a pair into one reading,
//! so [`fft_size_for`] exists specifically to keep the bin comfortably
//! narrower - see its own doc for the exact rule and why a version that
//! always returned the smallest allowed size would still "work" (every
//! test that only checks the function runs would pass) while quietly
//! failing the one job it has.

use alloc::vec;
use alloc::vec::Vec;

use crate::fft::fft_in_place;

/// The smallest FFT size [`fft_size_for`] will ever return, regardless of
/// how low `sample_rate` is.
pub const MIN_FFT_SIZE: usize = 256;

/// The largest FFT size [`fft_size_for`] will ever return, regardless of
/// how high `sample_rate` is - an unbounded size would make the waterfall
/// arbitrarily expensive to compute per block at a high device rate.
pub const MAX_FFT_SIZE: usize = 4096;

/// The widest a bin is allowed to be. The two Bell 103 pairs are 200 Hz
/// apart; 25 Hz gives eight bins of clearance either side of each tone,
/// comfortably narrower without demanding an FFT size so large it stops
/// being real-time on a modest device.
const MAX_BIN_HZ: u32 = 25;

/// The smallest power-of-two FFT size whose bin width
/// (`sample_rate / n`) is at most [`MAX_BIN_HZ`], clamped to
/// `[`[`MIN_FFT_SIZE`]`, `[`MAX_FFT_SIZE`]`]`.
///
/// `const fn` and pure integer arithmetic throughout - no floating point
/// division whose rounding could shift the answer by one power of two at
/// a boundary sample rate.
///
/// Pinned values (see this module's own tests): 512 at 8000 Hz (15.6 Hz
/// bins), 2048 at 48000 Hz (23.4 Hz bins). A version that silently
/// returned [`MIN_FFT_SIZE`] for every sample rate would still compile,
/// still return *a* power of two, and still pass any test that did not
/// check the actual number - which is why both of those values are
/// pinned as literals rather than derived from this function's own
/// constants.
pub const fn fft_size_for(sample_rate: u32) -> usize {
    // Ceiling division: the smallest n with n * MAX_BIN_HZ >= sample_rate,
    // i.e. the smallest n with sample_rate / n <= MAX_BIN_HZ.
    let min_n = (sample_rate as usize).div_ceil(MAX_BIN_HZ as usize);
    let mut n = MIN_FFT_SIZE;
    while n < min_n && n < MAX_FFT_SIZE {
        n *= 2;
    }
    n
}

/// The Hann window at sample `i` of `n`, `0.5 - 0.5*cos(2*pi*i/(n-1))` -
/// the periodic ripple a rectangular (unwindowed) block would otherwise
/// leak across every bin, which is what makes a waterfall of anything but
/// a bin-exact tone unreadable.
fn hann(i: usize, n: usize) -> f32 {
    if n <= 1 {
        return 1.0;
    }
    let angle = core::f64::consts::TAU * i as f64 / (n - 1) as f64;
    (0.5 - 0.5 * libm::cos(angle)) as f32
}

/// Hann-windows `samples`, runs [`fft_in_place`], and fills `out` with
/// `|X(k)|` for `k` in `0..out.len()`.
///
/// `samples.len()` must be a power of two (the whole block is the FFT
/// input - no zero-padding) and `out.len()` must equal `samples.len() /
/// 2`: the upper half of a real-input FFT mirrors the lower half, so
/// nothing beyond the Nyquist bin carries new information.
///
/// This is the one place in the FFT path that allocates - the windowed
/// copy and the FFT's own imaginary half both need a buffer sized to a
/// variable, caller-chosen block length. [`fft_in_place`] itself takes no
/// allocation at all; see that module's own doc.
pub fn magnitudes(samples: &[f32], out: &mut [f32]) {
    let n = samples.len();
    assert!(
        n.is_power_of_two(),
        "magnitudes: sample count {n} is not a power of two"
    );
    assert_eq!(
        out.len(),
        n / 2,
        "magnitudes: out.len() ({}) must be samples.len()/2 ({})",
        out.len(),
        n / 2
    );

    let mut re: Vec<f32> = samples
        .iter()
        .enumerate()
        .map(|(i, &s)| s * hann(i, n))
        .collect();
    let mut im: Vec<f32> = vec![0.0; n];
    fft_in_place(&mut re, &mut im);

    for (k, o) in out.iter_mut().enumerate() {
        *o = libm::hypotf(re[k], im[k]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- fft_size_for -----------------------------------------------------

    /// The two pinned values from the design spec's "Choosing N" section -
    /// see this module's own doc for why these are literals rather than
    /// anything derived from `fft_size_for`'s own constants.
    #[test]
    fn fft_size_for_is_512_at_8khz_and_2048_at_48khz() {
        assert_eq!(fft_size_for(8000), 512);
        assert_eq!(fft_size_for(48_000), 2048);
    }

    #[test]
    fn fft_size_for_bin_width_is_never_wider_than_25hz() {
        for &rate in &[8000u32, 11_025, 16_000, 22_050, 44_100, 48_000, 96_000] {
            let n = fft_size_for(rate);
            let bin_hz = rate as f64 / n as f64;
            assert!(
                bin_hz <= 25.0,
                "sample rate {rate}: n={n} gives a {bin_hz} Hz bin, wider than 25 Hz"
            );
        }
    }

    #[test]
    fn fft_size_for_is_always_a_power_of_two_within_the_clamp() {
        for rate in [0u32, 1, 100, 8000, 44_100, 48_000, 192_000, 1_000_000] {
            let n = fft_size_for(rate);
            assert!(
                n.is_power_of_two(),
                "rate {rate}: n={n} is not a power of two"
            );
            assert!(n >= MIN_FFT_SIZE, "rate {rate}: n={n} is below the clamp");
            assert!(n <= MAX_FFT_SIZE, "rate {rate}: n={n} is above the clamp");
        }
    }

    #[test]
    fn fft_size_for_clamps_at_the_top_for_a_very_high_sample_rate() {
        assert_eq!(fft_size_for(1_000_000), MAX_FFT_SIZE);
    }

    #[test]
    fn fft_size_for_clamps_at_the_bottom_for_a_very_low_sample_rate() {
        assert_eq!(fft_size_for(1), MIN_FFT_SIZE);
    }

    // --- magnitudes ---------------------------------------------------------

    fn sine(freq: f64, sample_rate: f64, n: usize) -> Vec<f32> {
        (0..n)
            .map(|i| libm::sin(core::f64::consts::TAU * freq * i as f64 / sample_rate) as f32)
            .collect()
    }

    fn peak_bin(mags: &[f32]) -> usize {
        mags.iter()
            .enumerate()
            .max_by(|a, b| a.1.partial_cmp(b.1).unwrap())
            .map(|(k, _)| k)
            .unwrap()
    }

    /// A bin-aligned tone puts the peak exactly where the maths says it
    /// should: `bin = freq * n / sample_rate`.
    #[test]
    fn a_bin_aligned_tone_peaks_at_its_own_bin() {
        const N: usize = 512;
        const SR: f64 = 8000.0;
        // Bin 64 is exactly 1000 Hz at this size and rate.
        let samples = sine(1000.0, SR, N);
        let mut mags = vec![0.0f32; N / 2];
        magnitudes(&samples, &mut mags);
        assert_eq!(peak_bin(&mags), 64);
    }

    /// Silence produces a flat, near-zero spectrum - not a NaN, not a
    /// spurious peak from window edge effects alone.
    #[test]
    fn silence_produces_a_near_zero_spectrum() {
        const N: usize = 256;
        let samples = vec![0.0f32; N];
        let mut mags = vec![0.0f32; N / 2];
        magnitudes(&samples, &mut mags);
        for (k, &m) in mags.iter().enumerate() {
            assert!(
                m.abs() < 1e-4,
                "bin {k} was {m}, expected near zero on silence"
            );
        }
    }

    #[test]
    #[should_panic(expected = "not a power of two")]
    fn magnitudes_panics_on_a_non_power_of_two_input() {
        let samples = vec![0.0f32; 300];
        let mut mags = vec![0.0f32; 150];
        magnitudes(&samples, &mut mags);
    }

    #[test]
    #[should_panic(expected = "out.len()")]
    fn magnitudes_panics_when_out_is_the_wrong_length() {
        let samples = vec![0.0f32; 256];
        let mut mags = vec![0.0f32; 100]; // should be 128
        magnitudes(&samples, &mut mags);
    }

    // --- Mutation-proof note ------------------------------------------------
    //
    // Mutation 3 (force `fft_size_for` to always return 256): this
    // module's own tests above (the 512/2048 pin and the bin-width sweep)
    // fail directly. The two-tone *display* resolution test that must
    // also fail lives in `modem-tui`'s waterfall tests, since resolving
    // two rows with a gap is a property of the rendered axis, not of this
    // function in isolation - see that crate's own mutation-proof note.
}
