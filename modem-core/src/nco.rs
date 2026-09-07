//! A numerically controlled oscillator.

use libm::sin;

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
}
