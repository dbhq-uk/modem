//! A radix-2, decimation-in-time FFT, computed in place.
//!
//! `no_std`, `libm` only, and - per this crate's own rule for the
//! waterfall - **no allocation in the transform itself**: bit-reversal is
//! an in-place swap and every butterfly stage reuses the same two slices,
//! so a caller with `re`/`im` already sized can call this on every audio
//! block without a single heap touch. [`crate::analyse::magnitudes`] is
//! where the one unavoidable allocation (windowing a variable-length input
//! into a fixed power-of-two buffer) lives instead.
//!
//! # Convention
//!
//! Forward transform, `X[k] = sum_n x[n] * exp(-i * 2 * pi * k * n / N)` -
//! the same sign convention a naive DFT written the obvious way uses. This
//! matters for anyone comparing a bin's imaginary part against a
//! hand-derived value (see this module's own tests): a conjugated twiddle
//! factor flips every imaginary part's sign and would still pass a test
//! that only checked magnitude.
//!
//! # Why hand-computed vectors, not a round trip
//!
//! An inverse FFT built from the same twiddle table as the forward one
//! passes a round-trip test even with the twiddles conjugated (both
//! directions flip together) or the bit-reversal dropped (both directions
//! skip it together) - the bug cancels itself out. Every test below
//! instead pins a literal array worked out by hand, or cross-checks
//! against a second, independently-written O(N^2) DFT - two different
//! pieces of code agreeing is evidence; one piece of code agreeing with
//! its own inverse is not. This project has shipped exactly this class of
//! self-consistent defect before (a reversed bit order in `Tx` that `Rx`
//! reversed straight back).

/// Reverses the lowest `bits` bits of `x`.
fn reverse_bits(mut x: usize, bits: u32) -> usize {
    let mut result = 0usize;
    for _ in 0..bits {
        result = (result << 1) | (x & 1);
        x >>= 1;
    }
    result
}

/// Swaps every `(i, j)` pair where `j` is `i` with its bits reversed -
/// the standard precondition for an iterative, in-place Cooley-Tukey FFT.
/// Dropping this and running the butterfly stages on the natural order
/// produces a permutation of the correct spectrum, not the spectrum
/// itself - see `bit_reversal_permutation_is_required` below.
fn bit_reverse_permute(re: &mut [f32], im: &mut [f32]) {
    let n = re.len();
    let bits = n.trailing_zeros();
    for i in 0..n {
        let j = reverse_bits(i, bits);
        if j > i {
            re.swap(i, j);
            im.swap(i, j);
        }
    }
}

/// Computes the FFT of `(re, im)` in place. `re.len()` must equal
/// `im.len()` and must be a power of two - both are programmer errors,
/// not runtime data conditions, so both panic rather than returning a
/// `Result` (matching this crate's existing convention, e.g.
/// `frame::encode_packet`'s length check).
///
/// A length of 0 or 1 is a no-op: there is nothing to permute or
/// butterfly, and the identity transform of a single sample is itself.
pub fn fft_in_place(re: &mut [f32], im: &mut [f32]) {
    assert_eq!(
        re.len(),
        im.len(),
        "fft_in_place: re and im must be the same length"
    );
    let n = re.len();
    assert!(
        n.is_power_of_two() || n == 0,
        "fft_in_place: length {n} is not a power of two"
    );
    if n <= 1 {
        return;
    }

    bit_reverse_permute(re, im);

    let mut size = 2usize;
    while size <= n {
        let half = size / 2;
        // exp(-i * 2*pi*k/size), the forward-transform convention this
        // module's own doc pins. Computed in f64 and narrowed to f32 only
        // at the point of use, so the angle itself never loses precision
        // to repeated f32 rounding across up to 4096 butterflies.
        let angle_step = -core::f64::consts::TAU / size as f64;
        for start in (0..n).step_by(size) {
            for k in 0..half {
                let angle = angle_step * k as f64;
                let tw_re = libm::cos(angle) as f32;
                let tw_im = libm::sin(angle) as f32;

                let i0 = start + k;
                let i1 = start + k + half;

                let xr = re[i1] * tw_re - im[i1] * tw_im;
                let xi = re[i1] * tw_im + im[i1] * tw_re;

                let ur = re[i0];
                let ui = im[i0];

                re[i0] = ur + xr;
                im[i0] = ui + xi;
                re[i1] = ur - xr;
                im[i1] = ui - xi;
            }
        }
        size *= 2;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;

    /// Tolerance for comparing against a hand-derived or independently
    /// computed value - loose enough to absorb f32 rounding across a few
    /// thousand butterflies, tight enough that a wrong bin (as opposed to
    /// a slightly-off one) still fails.
    const EPS: f32 = 1e-3;

    fn assert_close(actual: f32, expected: f32, msg: &str) {
        assert!(
            (actual - expected).abs() < EPS,
            "{msg}: expected {expected}, got {actual}"
        );
    }

    // --- Hand-computed vectors, N=4 --------------------------------------

    /// An impulse's DFT is flat: every bin has magnitude 1, because
    /// `X[k] = sum_n x[n] * exp(...) = x[0] = 1` for every k regardless of
    /// the twiddle factor at all. A test that only checked bin 0 would
    /// pass a transform that did nothing.
    #[test]
    fn n4_impulse_gives_unit_magnitude_in_every_bin() {
        let mut re = [1.0f32, 0.0, 0.0, 0.0];
        let mut im = [0.0f32; 4];
        fft_in_place(&mut re, &mut im);
        for k in 0..4 {
            let mag = (re[k] * re[k] + im[k] * im[k]).sqrt();
            assert_close(mag, 1.0, &format!("bin {k} magnitude"));
        }
    }

    /// DC: all energy lands in bin 0, nothing anywhere else. This is the
    /// test a scaled-by-N bug (or a scaled-by-1/N bug) cannot pass by
    /// accident alongside the impulse test above - the two pin different
    /// absolute scales (1 vs 4).
    #[test]
    fn n4_dc_gives_bin_zero_equal_to_n_and_nothing_else() {
        let mut re = [1.0f32, 1.0, 1.0, 1.0];
        let mut im = [0.0f32; 4];
        fft_in_place(&mut re, &mut im);
        assert_close(re[0], 4.0, "bin 0 real");
        assert_close(im[0], 0.0, "bin 0 imag");
        for k in 1..4 {
            assert_close(re[k], 0.0, &format!("bin {k} real"));
            assert_close(im[k], 0.0, &format!("bin {k} imag"));
        }
    }

    /// The Nyquist alternating sequence: all energy at bin N/2, which is
    /// the one bin the impulse and DC tests above never exercise.
    #[test]
    fn n4_nyquist_alternation_gives_bin_two_equal_to_n_and_nothing_else() {
        let mut re = [1.0f32, -1.0, 1.0, -1.0];
        let mut im = [0.0f32; 4];
        fft_in_place(&mut re, &mut im);
        assert_close(re[2], 4.0, "bin 2 real");
        assert_close(im[2], 0.0, "bin 2 imag");
        for k in [0usize, 1, 3] {
            assert_close(re[k], 0.0, &format!("bin {k} real"));
            assert_close(im[k], 0.0, &format!("bin {k} imag"));
        }
    }

    // --- Hand-computed vectors, N=8 --------------------------------------

    /// One full cycle of a cosine over 8 samples: real, positive energy
    /// split evenly between bin 1 and its mirror bin 7 (N - 1), nothing
    /// else. `x[n] = cos(2*pi*n/8)`.
    #[test]
    fn n8_one_cycle_cosine_gives_bins_one_and_seven_equal_to_four() {
        let mut re = [0.0f32; 8];
        let im_in = [0.0f32; 8];
        for (n, r) in re.iter_mut().enumerate() {
            *r = libm::cos(core::f64::consts::TAU * n as f64 / 8.0) as f32;
        }
        let mut im = im_in;
        fft_in_place(&mut re, &mut im);

        assert_close(re[1], 4.0, "bin 1 real");
        assert_close(im[1], 0.0, "bin 1 imag");
        assert_close(re[7], 4.0, "bin 7 real");
        assert_close(im[7], 0.0, "bin 7 imag");
        for k in [0usize, 2, 3, 4, 5, 6] {
            let mag = (re[k] * re[k] + im[k] * im[k]).sqrt();
            assert_close(mag, 0.0, &format!("bin {k} magnitude"));
        }
    }

    /// Two full cycles of a sine over 8 samples: `x[n] = sin(2*pi*2n/8)`.
    /// Energy lands at bin 2 and bin 6, magnitude 4 each - but unlike the
    /// cosine case, the imaginary parts carry a sign, and the two bins'
    /// signs are opposite: `X[2]` has a *negative* imaginary part and
    /// `X[6]` (its mirror) has a *positive* one. Conjugating the twiddle
    /// factor (`exp(+i...)` instead of `exp(-i...)`) flips every
    /// imaginary part's sign and would swap what this test expects at
    /// bin 2 for what it expects at bin 6 - magnitude alone cannot catch
    /// that, which is exactly why the brief calls this test out by name.
    #[test]
    fn n8_two_cycle_sine_signs_the_imaginary_part_correctly() {
        let mut re = [0.0f32; 8];
        for (n, r) in re.iter_mut().enumerate() {
            *r = libm::sin(core::f64::consts::TAU * 2.0 * n as f64 / 8.0) as f32;
        }
        let mut im = [0.0f32; 8];
        fft_in_place(&mut re, &mut im);

        assert_close(re[2], 0.0, "bin 2 real");
        assert_close(
            im[2],
            -4.0,
            "bin 2 imag (sign is the conjugated-twiddle tripwire)",
        );
        assert_close(re[6], 0.0, "bin 6 real");
        assert_close(
            im[6],
            4.0,
            "bin 6 imag (sign is the conjugated-twiddle tripwire)",
        );
        for k in [0usize, 1, 3, 4, 5, 7] {
            let mag = (re[k] * re[k] + im[k] * im[k]).sqrt();
            assert_close(mag, 0.0, &format!("bin {k} magnitude"));
        }
    }

    // --- Cross-check against an independently written naive O(N^2) DFT --

    /// A second, deliberately separate implementation of the same
    /// convention (`X[k] = sum_n x[n] * exp(-i*2*pi*k*n/N)`), written
    /// straight from the definition rather than derived from
    /// `fft_in_place` in any way. Agreement between this and the radix-2
    /// transform on random data is evidence the fast path is correct;
    /// agreement between the fast path and its own inverse would not be.
    fn naive_dft(re_in: &[f32], im_in: &[f32]) -> (Vec<f32>, Vec<f32>) {
        let n = re_in.len();
        let mut ore = Vec::with_capacity(n);
        let mut oim = Vec::with_capacity(n);
        for k in 0..n {
            let mut sr = 0.0f64;
            let mut si = 0.0f64;
            for t in 0..n {
                let angle = -core::f64::consts::TAU * (k * t) as f64 / n as f64;
                let c = libm::cos(angle);
                let s = libm::sin(angle);
                let xr = re_in[t] as f64;
                let xi = im_in[t] as f64;
                sr += xr * c - xi * s;
                si += xr * s + xi * c;
            }
            ore.push(sr as f32);
            oim.push(si as f32);
        }
        (ore, oim)
    }

    /// A private xorshift64, seeded and stepped exactly like
    /// `impair.rs`'s own (that module's own doc explains the choice: a
    /// fixed seed reproduces a fixed result without pulling in a `rand`
    /// dependency or leaving `no_std`). Independent of `impair.rs` itself
    /// so this test file has no dependency on it.
    fn xorshift_next(state: &mut u64) -> u64 {
        let mut x = *state;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        *state = x;
        x
    }

    #[test]
    fn n64_random_data_matches_an_independent_naive_dft() {
        let mut state = 0x243F_6A88_85A3_08D3u64; // arbitrary, fixed - reproducible
        let mut re: Vec<f32> = (0..64)
            .map(|_| {
                let v = xorshift_next(&mut state);
                ((v >> 11) as f64 / (1u64 << 53) as f64) as f32 * 2.0 - 1.0
            })
            .collect();
        let mut im = re.clone();
        // Give im its own independent random values rather than reusing
        // re's - a transform that quietly assumed a real-only input (e.g.
        // ignored `im` on the way in) would still pass on a real-valued
        // signal.
        for v in im.iter_mut() {
            let r = xorshift_next(&mut state);
            *v = ((r >> 11) as f64 / (1u64 << 53) as f64) as f32 * 2.0 - 1.0;
        }

        let (want_re, want_im) = naive_dft(&re, &im);
        fft_in_place(&mut re, &mut im);

        for k in 0..64 {
            assert_close(re[k], want_re[k], &format!("bin {k} real"));
            assert_close(im[k], want_im[k], &format!("bin {k} imag"));
        }
    }

    // --- Mutation-proof notes ---------------------------------------------
    //
    // Mutation 1 (conjugate the twiddle - flip the sign of `angle_step`):
    // caught by `n8_two_cycle_sine_signs_the_imaginary_part_correctly`,
    // which is the one hand-computed vector whose expected output is not
    // symmetric under conjugation (the cosine and impulse/DC/Nyquist
    // vectors all have zero or symmetric imaginary parts and would not
    // notice). See the task report for the actual failing transcript.
    //
    // Mutation 2 (drop `bit_reverse_permute`): NOT caught by the impulse
    // vector (`n4_impulse_gives_unit_magnitude_in_every_bin`) - a single
    // 1 at index 0 is a fixed point of bit-reversal (`reverse_bits(0) ==
    // 0`), so that test passes whether or not the permutation runs, which
    // is worth recording since it is exactly the kind of self-consistent
    // false pass this project keeps warning about. It also is not caught
    // by the DC vector, whose four equal values are likewise invariant
    // under any reordering. It IS caught by
    // `n4_nyquist_alternation_gives_bin_two_equal_to_n_and_nothing_else`:
    // worked by hand, skipping the permutation on `[1,-1,1,-1]` produces
    // `re = [0,2,0,2]`, `im = [0,-2,0,2]` instead of the correct
    // `[0,0,4,0]` / `[0,0,0,0]` - see the task report for the transcript.

    #[test]
    fn zero_and_one_length_are_a_no_op() {
        let mut re: [f32; 0] = [];
        let mut im: [f32; 0] = [];
        fft_in_place(&mut re, &mut im); // must not panic

        let mut re1 = [3.5f32];
        let mut im1 = [-1.25f32];
        fft_in_place(&mut re1, &mut im1);
        assert_eq!(re1[0], 3.5);
        assert_eq!(im1[0], -1.25);
    }

    #[test]
    #[should_panic(expected = "not a power of two")]
    fn non_power_of_two_length_panics() {
        let mut re = [0.0f32; 3];
        let mut im = [0.0f32; 3];
        fft_in_place(&mut re, &mut im);
    }
}
