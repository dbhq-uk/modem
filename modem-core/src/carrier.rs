//! Carrier detection.
//!
//! Two thresholds with a gap between them stop the detector chattering at
//! the boundary, and a hold-off keeps carrier up through the gaps between
//! characters. Without the hold-off, a quiet moment reads as NO CARRIER.
//!
//! The floor is a live estimate of ambient energy, not a fixed threshold,
//! but it only ever moves while carrier is *not* asserted, and freezes
//! solid the instant it is. Freezing is what makes a sustained real
//! carrier safe to hold indefinitely - see `update`'s comment for the two
//! designs that got this wrong first, both found by running the code for
//! long enough, not by inspection. Tracking while undetected is what lets
//! the detector calibrate to whatever room it is actually in, rather than
//! to whatever the very first samples happened to look like.
//!
//! This does not make the detector immune to loud ambient noise. A
//! receiver that powers up straight into a noise floor loud enough to
//! itself clear RISE_RATIO against the starting guess locks onto it as
//! carrier inside the first attack time constant - milliseconds - before
//! FLOOR_ADAPT has had any chance to react, and then holds that lock
//! indefinitely, because holding is exactly what correctly protects a real
//! carrier from a burst of noise. No single energy-ratio threshold can
//! tell those two cases apart from a cold start; the only real answer
//! generally is comparing against known tone frequencies (a Goertzel bank)
//! rather than raw in-band energy, which is a larger change than this
//! task, and not one made here. `rx.rs`'s dead-line noise sweep measures
//! where that ceiling currently sits.

use crate::DSP_RATE;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Event {
    CarrierUp,
    CarrierDown,
}

/// In-band energy this many times the noise floor to assert carrier.
const RISE_RATIO: f64 = 4.0;
/// And must fall below this to deassert. The gap is the hysteresis.
const FALL_RATIO: f64 = 2.0;
/// Smoothing for the signal level. At DSP_RATE = 8 kHz this is a 12.5 ms
/// time constant (1 / (ATTACK * DSP_RATE)).
const ATTACK: f64 = 0.01;
/// Smoothing for the floor while carrier is not detected. A 1.25 s time
/// constant (1 / (FLOOR_ADAPT * DSP_RATE)) - fast enough to characterise a
/// room's ambient level well inside an ordinary quiet spell between calls.
///
/// This constant only ever runs while `!detected` (see `update`), which is
/// what makes a value this fast safe. Task 6's first attempt used one
/// constant for both states and had to choose between two failures: fast
/// enough to calibrate to the room, and a sustained carrier converges the
/// floor onto itself and drops (measured: 14.4 minutes of unbroken idle
/// mark at a "far slower" 1e-7 was still enough); or slow enough to survive
/// a sustained carrier, and it can no longer track a noisy room's real
/// level, which is what let noise above roughly -38 dBFS assert carrier
/// and never release it. Splitting the two states removes the trade-off:
/// FLOOR_ADAPT only has to be fast enough to be useful, because freezing -
/// not slowness - is what protects it once carrier is up.
const FLOOR_ADAPT: f64 = 1e-4;
/// Starting guess for the noise floor before any samples have calibrated
/// it, and the effective ceiling on how loud an ambient a cold start can
/// reject: energy above `RISE_RATIO * INITIAL_FLOOR` clears the threshold
/// in the first attack time constant, before FLOOR_ADAPT has moved this at
/// all, and then freezes there the moment it does. Measured against this
/// crate's noise sweep: 1e-3 clears -38 dBFS (amplitude 0.0126, "ordinary
/// microphone noise floor" - measured correlator energy 3.8e-3, ratio 3.8,
/// under RISE_RATIO) but not -33 dBFS and louder (amplitude 0.02 and
/// above). That is the real, current ceiling; see `rx.rs`'s sweep test for
/// the measured numbers and the module doc above for why no choice of
/// constant removes it, only moves it.
const INITIAL_FLOOR: f64 = 1e-3;
/// How long the energy must stay low before carrier drops. Longer than any
/// inter-character gap at 300 baud, which is at most a few symbol times.
const HOLDOFF_SECONDS: f64 = 0.5;
/// Added to the floor before dividing, so a floor that has decayed to
/// (near enough) zero during genuine silence divides safely instead of
/// producing an infinite or NaN ratio. Negligible next to INITIAL_FLOOR
/// and every real energy value in this module's tests, so it never
/// perturbs a real ratio - it only guards the one degenerate case.
const FLOOR_EPSILON: f64 = 1e-12;

pub struct CarrierDetector {
    level: f64,
    floor: f64,
    detected: bool,
    holdoff: u32,
    holdoff_samples: u32,
}

impl Default for CarrierDetector {
    fn default() -> Self {
        Self::new()
    }
}

impl CarrierDetector {
    pub fn new() -> Self {
        Self {
            level: 0.0,
            floor: INITIAL_FLOOR,
            detected: false,
            holdoff: 0,
            holdoff_samples: (HOLDOFF_SECONDS * DSP_RATE) as u32,
        }
    }

    pub fn detected(&self) -> bool {
        self.detected
    }

    /// Feeds one sample's total in-band energy. Returns an event on a
    /// transition, otherwise None.
    pub fn update(&mut self, energy: f64) -> Option<Event> {
        self.level += ATTACK * (energy - self.level);

        // The floor moves only while carrier is not asserted, and freezes
        // solid the instant it is - not slowly, not eventually, not at
        // all. This is the third attempt at this line, and the first two
        // are worth naming so nobody reintroduces them:
        //
        // 1. Fast down / slow up (Task 6's very first version): only the
        //    downward branch was fast. The upward branch - the one a
        //    sustained carrier actually exercises - was already the same
        //    slow constant as design 2 below, so this never protected
        //    against a sustained carrier at all; it only ratcheted the
        //    floor towards a noisy signal's dips, which is a different
        //    bug (measured: 42 fabricated bytes from five seconds of
        //    noise that never contained a carrier).
        // 2. One slow constant, unconditionally (Task 6's second version,
        //    landed and reviewed): removes the ratchet, but "slow" is not
        //    "never" - any positive rate converges given enough time.
        //    Measured directly: constant energy 0.5 held for 167 minutes
        //    produced CarrierUp at sample 0 and CarrierDown at sample
        //    6,915,450 - 14.4 minutes of perfectly ordinary unbroken
        //    carrier, gone, with no way back for the rest of the session.
        //
        // Freezing is not a slower version of design 2; it is a different
        // claim - the floor does not move here, at any rate, for any
        // duration, while detected is true - and it is the only version
        // of the three that is actually independent of how long carrier
        // stays up.
        if !self.detected {
            self.floor += FLOOR_ADAPT * (energy - self.floor);
        }

        let ratio = self.level / (self.floor + FLOOR_EPSILON);
        if !self.detected {
            if ratio > RISE_RATIO {
                self.detected = true;
                self.holdoff = 0;
                return Some(Event::CarrierUp);
            }
            return None;
        }

        if ratio < FALL_RATIO {
            self.holdoff += 1;
            if self.holdoff >= self.holdoff_samples {
                self.detected = false;
                self.holdoff = 0;
                return Some(Event::CarrierDown);
            }
        } else {
            self.holdoff = 0;
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The gap between FALL_RATIO and RISE_RATIO is what makes a ratio
    /// resting anywhere between them stable rather than a coin flip.
    /// Mutation-proven: setting FALL_RATIO equal to RISE_RATIO (removing
    /// the gap) passes every test in this crate's suite untouched - byte
    /// counts, framing error counts and the carrier tests all stay green,
    /// because none of them ever park the ratio inside where the gap used
    /// to be. This drives it there directly.
    #[test]
    fn ratio_inside_the_hysteresis_gap_does_not_drop_carrier() {
        let mut c = CarrierDetector::new();

        // Establish carrier on a strong signal. The floor freezes on the
        // very first sample - level alone clears RISE_RATIO against
        // INITIAL_FLOOR before FLOOR_ADAPT has moved it at all.
        let mut got_up = false;
        for _ in 0..10 {
            if c.update(1.0) == Some(Event::CarrierUp) {
                got_up = true;
            }
        }
        assert!(got_up, "carrier never asserted on a strong signal");

        // Hold the ratio at roughly 3 - inside (FALL_RATIO, RISE_RATIO)
        // against the now-frozen floor - for a full second, twice the
        // 0.5 s hold-off, so a spurious drop has every opportunity to
        // show itself.
        for i in 0..8000 {
            let ev = c.update(3.0e-3);
            assert_ne!(
                ev,
                Some(Event::CarrierDown),
                "carrier dropped at sample {i} while the ratio sat inside the hysteresis gap"
            );
        }
        assert!(
            c.detected(),
            "carrier not detected after holding the ratio inside the gap"
        );
    }

    /// A cross-check on the sum-vs-difference design decision at the unit
    /// that embodies it, not proof of the `rx.rs` wiring. Earlier drafts
    /// of this file claimed a fed-the-difference tie could not be
    /// reproduced through the real correlator at all, blaming Bell 103's
    /// 200 Hz mark/space spacing and this correlator's window length.
    /// That was wrong - see `rx.rs::tests::two_tones_at_equal_strength_
    /// raise_carrier`, which reproduces it directly through `Rx::write`
    /// and is the real proof, precisely because it can fail against a
    /// wiring defect and this test cannot: feeding a `CarrierDetector` a
    /// pre-computed `0.0` can never distinguish "the wiring sums" from
    /// "the wiring differences", it only shows what this detector does
    /// with whatever number it is handed.
    #[test]
    fn a_tie_between_strong_signals_is_not_a_tie_between_noise_floors() {
        let mut fed_the_difference_weak = CarrierDetector::new();
        let mut fed_the_difference_strong = CarrierDetector::new();
        let mut fed_the_sum_strong = CarrierDetector::new();

        // Two ties: mark and space equal at a noise-floor magnitude, and
        // mark and space equal at a strong-signal magnitude. `soft` -
        // mark minus space - is zero in both cases; energy - the sum -
        // is not.
        let (weak_m, weak_s): (f64, f64) = (1e-4, 1e-4);
        let (strong_m, strong_s): (f64, f64) = (0.5, 0.5);

        for _ in 0..8000 {
            fed_the_difference_weak.update(weak_m - weak_s);
            fed_the_difference_strong.update(strong_m - strong_s);
            fed_the_sum_strong.update((strong_m + strong_s) / 27.0);
        }

        assert!(
            !fed_the_difference_weak.detected(),
            "a weak tie must not read as carrier"
        );
        assert!(
            !fed_the_difference_strong.detected(),
            "fed the difference, a strong tie reads exactly like the weak one above"
        );
        assert!(
            fed_the_sum_strong.detected(),
            "fed the sum, the same strong tie correctly raises carrier"
        );
    }

    /// Critical 1, pinned directly at the mechanism that failed. Measured
    /// against the previous design: constant energy 0.5 held for 167
    /// minutes produced CarrierUp at sample 0 and CarrierDown at sample
    /// 6,915,450 - 14.4 minutes in. This holds the same constant energy
    /// for 20 minutes, comfortably past that failure point, and a frozen
    /// floor makes it provably duration-independent - if this passes at
    /// 20 minutes under the current design it passes forever, because
    /// nothing here is a function of elapsed time once detected.
    ///
    /// Runs directly against `CarrierDetector`, not through `Rx`/`Tx`:
    /// the same duration through the full DSP pipeline takes on the order
    /// of ten seconds of wall-clock time, and the mechanism under test -
    /// whether the floor moves while detected - has nothing to do with
    /// the correlator or resampler.
    #[test]
    fn carrier_survives_twenty_minutes_of_unbroken_energy() {
        let mut c = CarrierDetector::new();
        let mut got_up = false;
        for i in 0..(20 * 60 * 8000) {
            match c.update(0.5) {
                Some(Event::CarrierUp) => got_up = true,
                Some(Event::CarrierDown) => {
                    panic!("carrier dropped at sample {i} of 20 minutes of unbroken energy")
                }
                None => {}
            }
        }
        assert!(got_up, "carrier never asserted on a strong signal");
        assert!(c.detected(), "carrier not detected after 20 minutes");
    }

    /// Pins the ordering of the two thresholds directly, and demonstrates
    /// why: a ratio resting between them behaves oppositely depending on
    /// which one is bigger. With RISE_RATIO > FALL_RATIO (correct), a
    /// ratio below both simply never asserts carrier. Invert them - the
    /// reviewer's example was RISE_RATIO 4.0 -> 1.5 with FALL_RATIO left
    /// at 2.0 - and that same ratio clears the (now lower) rise threshold
    /// immediately, then the (now higher, relatively) fall threshold
    /// immediately un-clears it once the hold-off elapses, forever, since
    /// nothing about the input ever changes. `carrier_detected()` sampled
    /// once at the end cannot see this - it depends on parity, so it
    /// happens to read the same either way half the time. Counting events
    /// over several hold-off periods cannot be fooled by parity.
    #[test]
    fn hysteresis_must_not_invert() {
        // Checked at compile time - RISE_RATIO and FALL_RATIO are both
        // const, so the comparison has no runtime dependency to observe.
        // A mutation that inverts them fails the build outright, before
        // this or any other test runs.
        const _: () = assert!(
            RISE_RATIO > FALL_RATIO,
            "RISE_RATIO must be strictly greater than FALL_RATIO or the two \
             thresholds invert and the detector chatters every hold-off period"
        );

        let mut c = CarrierDetector::new();
        // A ratio against INITIAL_FLOOR that a correct configuration
        // never asserts on at all (1.75 clears neither RISE_RATIO=4 nor
        // FALL_RATIO=2), held for 4 s - eight hold-off periods, so a
        // chattering configuration gets many chances to show it.
        let mut events = 0u32;
        for _ in 0..32_000 {
            if c.update(1.75 * INITIAL_FLOOR).is_some() {
                events += 1;
            }
        }
        assert!(
            events <= 1,
            "expected zero transitions for a ratio that never clears RISE_RATIO, got {events}"
        );
    }

    /// Pins the hold-off as counting *consecutive* low samples, not an
    /// unbounded cumulative total. Deleting the `else { self.holdoff = 0
    /// }` branch keeps the counter incrementing across separate dips with
    /// no reset in between, so many short dips - each individually far too
    /// brief to matter - add up to a drop that consecutive counting would
    /// never produce.
    ///
    /// The recovery phase is deliberately a ratio of 3 - above FALL_RATIO
    /// so it can reset the hold-off, but below RISE_RATIO so a *wrongly*
    /// dropped detector does not immediately reassert on it and mask the
    /// bug by the time the test checks the final state. And the dip and
    /// recovery lengths are not arbitrary: level takes about one attack
    /// time constant to cross a threshold after a step, so a phase much
    /// shorter than that never actually gets the ratio to the other side
    /// of FALL_RATIO at all, dip or recovery, and the mutation this test
    /// exists to catch passes by accident rather than by the fix working -
    /// measured directly, 100-sample phases did exactly that.
    #[test]
    fn holdoff_is_consecutive_not_cumulative() {
        let mut c = CarrierDetector::new();
        let mut got_up = false;
        for _ in 0..10 {
            if c.update(1.0) == Some(Event::CarrierUp) {
                got_up = true;
            }
        }
        assert!(got_up, "carrier never asserted on a strong signal");

        // 40 cycles of a 200-sample dip (ratio ~0.1, well under FALL_RATIO)
        // and a 200-sample recovery (ratio 3, between FALL_RATIO and
        // RISE_RATIO). Consecutive counting resets every recovery and
        // never approaches the hold-off; cumulative counting only ever
        // increments during the dips and reaches the 4000-sample hold-off
        // around cycle 25.
        for cycle in 0..40 {
            for _ in 0..200 {
                let ev = c.update(0.1 * INITIAL_FLOOR); // ratio ~0.1
                assert_ne!(
                    ev,
                    Some(Event::CarrierDown),
                    "dropped during dip {cycle}, which alone is far shorter than the hold-off"
                );
            }
            for _ in 0..200 {
                c.update(3.0 * INITIAL_FLOOR);
            }
        }
        assert!(
            c.detected(),
            "carrier dropped from many short dips that never individually reached the hold-off"
        );
    }
}
