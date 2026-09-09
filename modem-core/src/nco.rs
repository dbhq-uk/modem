//! A numerically controlled oscillator, and the Goertzel filter that
//! answers the reverse question - not "generate this frequency" but "how
//! much of this exact frequency is in what I was just handed".

use libm::{cos, sin};

/// Keeps a running phase so that changing frequency never produces a
/// discontinuity. A phase jump splatters energy across the spectrum and
/// would land in the other direction's band, which is exactly the thing
/// Bell 103's split-band design relies on not happening.
pub struct Nco {
    phase: f64,
    inc: f64,
    sample_rate: f64,
}

impl Nco {
    pub fn new(freq: f64, sample_rate: f64) -> Self {
        Self {
            phase: 0.0,
            inc: core::f64::consts::TAU * freq / sample_rate,
            sample_rate,
        }
    }

    /// Changes frequency while preserving phase.
    pub fn set_freq(&mut self, freq: f64) {
        self.inc = core::f64::consts::TAU * freq / self.sample_rate;
    }

    /// Advances one sample and returns its amplitude in [-1, 1].
    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> f64 {
        let v = sin(self.phase);
        self.phase += self.inc;
        if self.phase >= core::f64::consts::TAU {
            self.phase -= core::f64::consts::TAU;
        }
        v
    }

    pub fn reset(&mut self) {
        self.phase = 0.0;
    }
}

/// A streaming, single-frequency Goertzel filter: the production
/// counterpart to the test-only [`goertzel`] free function below, which
/// needs a whole buffer up front and is only ever used to check what a
/// rendered signal actually contains. This is the other direction - fed
/// one sample at a time, for as long as the caller likes, with no buffer
/// and no allocation, which is what [`crate::carrier::ToneDominance`]
/// needs to run inside `Rx`'s per-sample streaming path (`no_std`, no
/// heap touch after construction).
///
/// Classic Goertzel: two running accumulators (`s1`, `s2`) replace the
/// whole-buffer correlation a naive single-bin DFT would need, at the
/// cost of only being able to read the block's total power back, not a
/// running one - `reset` starts the next block. This is exactly the
/// trade-off [`ToneDominance`](crate::carrier::ToneDominance) wants: it
/// judges one fixed-length block at a time, never a sliding window.
pub struct Goertzel {
    coeff: f64,
    s1: f64,
    s2: f64,
}

impl Goertzel {
    /// `freq` in Hz, `sample_rate` in Hz - the same units [`Nco::new`]
    /// takes, and deliberately not tied to any particular block length:
    /// unlike an FFT bin, a Goertzel filter's target frequency does not
    /// need to land on `k * sample_rate / n` for any block length `n`.
    pub fn new(freq: f64, sample_rate: f64) -> Self {
        let w = core::f64::consts::TAU * freq / sample_rate;
        Self {
            coeff: 2.0 * cos(w),
            s1: 0.0,
            s2: 0.0,
        }
    }

    /// Feeds one sample into the current block.
    pub fn feed(&mut self, x: f64) {
        let s0 = x + self.coeff * self.s1 - self.s2;
        self.s2 = self.s1;
        self.s1 = s0;
    }

    /// The power (magnitude squared) of whatever has been [`feed`](Self::feed)
    /// since the last [`reset`](Self::reset). The standard real-valued
    /// Goertzel power formula - `s1^2 + s2^2 - coeff*s1*s2` - which is
    /// `|X(f)|^2` for a length-`n` single-bin DFT at this filter's
    /// frequency, without ever forming a complex number: unneeded here,
    /// since [`crate::carrier::ToneDominance`] only ever wants a power to
    /// compare against another power, never a phase.
    pub fn power(&self) -> f64 {
        self.s1 * self.s1 + self.s2 * self.s2 - self.coeff * self.s1 * self.s2
    }

    /// Starts a fresh block: the two accumulators do not decay or forget
    /// on their own, so a caller running fixed-length blocks (as
    /// [`crate::carrier::ToneDominance`] does) must call this once per
    /// block, not rely on `feed` alone to keep the estimate current.
    pub fn reset(&mut self) {
        self.s1 = 0.0;
        self.s2 = 0.0;
    }
}

#[cfg(test)]
pub(crate) fn goertzel(samples: &[f64], target: f64, sample_rate: f64) -> f64 {
    let n = samples.len();
    let k = (0.5 + n as f64 * target / sample_rate) as usize;
    let w = core::f64::consts::TAU * k as f64 / n as f64;
    let cosw = w.cos();
    let coeff = 2.0 * cosw;
    let (mut s1, mut s2) = (0.0f64, 0.0f64);
    for &x in samples {
        let s0 = x + coeff * s1 - s2;
        s2 = s1;
        s1 = s0;
    }
    let real = s1 - s2 * cosw;
    let imag = s2 * w.sin();
    real.hypot(imag) / (n as f64 / 2.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The largest possible difference between two adjacent samples of sin
    /// at this frequency. A hardcoded bound is wrong: at 2225 Hz and 8 kHz
    /// the real bound is 1.53, so a fixed 1.0 would fail a correct
    /// implementation.
    fn step_bound(freq: f64, sample_rate: f64) -> f64 {
        2.0 * (core::f64::consts::PI * freq / sample_rate).sin().abs()
    }

    #[test]
    fn produces_requested_frequency() {
        const FS: f64 = 8000.0;
        let mut n = Nco::new(1270.0, FS);
        let buf: alloc::vec::Vec<f64> = (0..8000).map(|_| n.next()).collect();
        assert!(goertzel(&buf, 1270.0, FS) >= 0.9);
        assert!(goertzel(&buf, 2225.0, FS) <= 0.05);
    }

    #[test]
    fn phase_is_continuous_across_frequency_change() {
        const FS: f64 = 8000.0;
        let mut n = Nco::new(1070.0, FS);
        for _ in 0..100 {
            n.next();
        }
        let before = n.next();

        n.set_freq(2225.0);
        // next() returns sin(phase) then advances, so the first sample
        // after set_freq still sits at the phase the old increment left
        // behind. The one after it is the first to carry the new
        // increment. Comparing only before/first tests the old frequency
        // twice and proves nothing.
        let first = n.next();
        let second = n.next();

        assert!((first - before).abs() <= step_bound(1070.0, FS) + 1e-9);
        assert!((second - first).abs() <= step_bound(2225.0, FS) + 1e-9);
    }

    /// set_freq must actually retune. The continuity test above only bounds
    /// the maximum step, which a too-small increment satisfies trivially,
    /// so without this a set_freq using the wrong sample rate passes every
    /// other test in the file.
    #[test]
    fn set_freq_retunes_to_the_new_frequency() {
        const FS: f64 = 8000.0;
        let mut n = Nco::new(1070.0, FS);
        for _ in 0..100 {
            n.next();
        }
        n.set_freq(2225.0);
        let buf: alloc::vec::Vec<f64> = (0..8000).map(|_| n.next()).collect();
        assert!(
            goertzel(&buf, 2225.0, FS) >= 0.9,
            "did not retune to 2225 Hz"
        );
        assert!(goertzel(&buf, 1070.0, FS) <= 0.05, "still emitting 1070 Hz");
    }

    #[test]
    fn reset_returns_phase_to_zero() {
        let mut n = Nco::new(1270.0, 8000.0);
        for _ in 0..37 {
            n.next();
        }
        n.reset();
        // next() samples before advancing, so a zeroed phase gives sin(0).
        assert_eq!(n.next(), 0.0);
    }

    /// The regression the bounded-step test cannot catch alone. A set_freq
    /// that zeroed phase would leave the next sample at exactly sin(0).
    #[test]
    fn set_freq_does_not_reset_phase() {
        let mut n = Nco::new(1070.0, 8000.0);
        for _ in 0..100 {
            n.next();
        }
        n.set_freq(2225.0);
        assert_ne!(n.next(), 0.0, "phase was reset");
    }

    #[test]
    fn phase_does_not_drift_over_ten_seconds() {
        const FS: f64 = 8000.0;
        let mut n = Nco::new(1000.0, FS);
        let buf: alloc::vec::Vec<f64> = (0..80_000).map(|_| n.next()).collect();
        let tail = goertzel(&buf[72_000..], 1000.0, FS);

        let mut r = Nco::new(1000.0, FS);
        let refbuf: alloc::vec::Vec<f64> = (0..8000).map(|_| r.next()).collect();
        let want = goertzel(&refbuf, 1000.0, FS);

        assert!((tail - want).abs() <= 0.01, "drifted: {tail} vs {want}");
    }

    // --- Goertzel (streaming) ---------------------------------------------

    /// `Goertzel::power` and the test-only block-based `goertzel` above
    /// share the same `s1`/`s2` recursion, so this is not two independent
    /// derivations agreeing (see `fft.rs`'s own doc on why that
    /// distinction matters) - it is a wiring check: feeding one sample at
    /// a time through `feed` must reach the identical final state as
    /// processing the whole block in one pass. Algebraically,
    /// `power() == (real^2 + imag^2)` for the block-based version's own
    /// `real`/`imag` (expand `(s1 - s2*cosw)^2 + (s2*sinw)^2` and the
    /// cross terms collapse to `s1^2 + s2^2 - coeff*s1*s2` exactly), so
    /// `power()` must equal `(goertzel(...) * n/2)^2` to within float
    /// rounding.
    #[test]
    fn streaming_goertzel_matches_the_block_based_helper() {
        const FS: f64 = 8000.0;
        const FREQ: f64 = 1270.0;
        // The block-based helper snaps `target` to the nearest exact DFT
        // bin for whatever `n` it is given (`k = round(n*target/rate)`) -
        // deliberately not what the streaming filter does (see its own
        // doc: it targets the exact frequency regardless of block
        // length), so the two would read slightly different actual
        // frequencies at a block length that does not land 1270 Hz on a
        // bin exactly. 800 is the smallest length that does: 1270/8000 =
        // 127/800 in lowest terms, so `k = 127` is exact here with no
        // rounding on either side, and both filters are provably
        // comparing the identical frequency.
        const N: usize = 800;
        let mut nco = Nco::new(FREQ, FS);
        let samples: alloc::vec::Vec<f64> = (0..N).map(|_| nco.next()).collect();

        let mut g = Goertzel::new(FREQ, FS);
        for &x in &samples {
            g.feed(x);
        }

        let want_mag = goertzel(&samples, FREQ, FS) * (N as f64 / 2.0);
        let want_power = want_mag * want_mag;
        assert!(
            (g.power() - want_power).abs() < 1e-6 * want_power.max(1.0),
            "streaming power {} did not match block-based power {}",
            g.power(),
            want_power
        );
    }

    /// A full-scale tone's power must dwarf a tone 200 Hz away (Bell
    /// 103's own mark/space spacing) - the property `ToneDominance`
    /// actually leans on to tell mark from space and either from noise.
    #[test]
    fn streaming_goertzel_rejects_a_tone_200hz_away() {
        const FS: f64 = 8000.0;
        const N: usize = 256;
        let mut nco = Nco::new(1270.0, FS);
        let samples: alloc::vec::Vec<f64> = (0..N).map(|_| nco.next()).collect();

        let mut on_target = Goertzel::new(1270.0, FS);
        let mut off_target = Goertzel::new(1070.0, FS);
        for &x in &samples {
            on_target.feed(x);
            off_target.feed(x);
        }
        assert!(
            on_target.power() > 100.0 * off_target.power(),
            "on-target power {} was not far above off-target power {}",
            on_target.power(),
            off_target.power()
        );
    }

    /// `reset` must actually zero the accumulators, not merely look like
    /// it worked because the next block happens to be quiet too - fed a
    /// loud block first, then reset, then read back before feeding
    /// anything new.
    #[test]
    fn reset_zeroes_the_accumulators() {
        let mut g = Goertzel::new(1270.0, 8000.0);
        let mut nco = Nco::new(1270.0, 8000.0);
        for _ in 0..256 {
            g.feed(nco.next());
        }
        assert!(
            g.power() > 0.0,
            "did not accumulate any power to begin with"
        );
        g.reset();
        assert_eq!(g.power(), 0.0, "reset did not zero the accumulators");
    }
}
