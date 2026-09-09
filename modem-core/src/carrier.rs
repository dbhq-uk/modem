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
//! to whatever the very first samples happened to look like, and
//! `FLOOR_MIN_FRACTION` bounds how far down that tracking can go - see its
//! own comment for why the bound exists and is a measured trade-off, not a
//! safety margin picked for comfort.
//!
//! Sensitivity and noise rejection are therefore both functions of how
//! long the line has been idle, converging to a stable pair of values by
//! about 10 s (eight `FLOOR_ADAPT` time constants) and staying there
//! indefinitely - the clamp means they do not keep drifting after that.
//! Measured at `FLOOR_MIN_FRACTION = 1/10`: a cold start (no prior idle)
//! rejects noise up to about 0.0175-0.0205 (a range across seeds and both
//! Bell 103 bands, not one precise number - see `INITIAL_FLOOR`'s own
//! comment) and detects a clean signal down to about 0.0075; after 10 s or
//! more of idle line, rejection tightens to about 0.002-0.0025 and
//! sensitivity improves to about 0.0005-0.0007. Both numbers move in the
//! *same* direction with idle time (more sensitive, less tolerant of loud
//! ambient) because they are two readings of the one mechanism - the floor
//! decaying towards whatever it is actually fed - not two independent
//! properties. Only the cold-start noise ceiling has been checked across
//! seeds and bands for this range-not-a-point caveat; the other three
//! figures here are each still a single measured draw.
//!
//! Raw energy alone cannot tell loud-ambient-noise-from-the-first-sample
//! apart from genuine-carrier-from-the-first-sample: a receiver that
//! powers up straight into a noise floor loud enough to itself clear
//! RISE_RATIO against the floor's current value - the starting guess at a
//! cold start, or the clamped minimum after an idle spell - would lock
//! onto it as carrier inside the first attack time constant, milliseconds,
//! before FLOOR_ADAPT has had any chance to react. This is exactly what
//! this project's own off-hook click (a short decaying broadband
//! transient, RMS about 0.085 - see `overture.rs`'s `click_sample`) did to
//! the far end's receiver before [`ToneDominance`] existed: the click has
//! nothing to do with either Bell 103 band, but it is loud, and loud is
//! all `update` alone can see. `rx.rs`'s noise-rejection tests measure
//! where this floor-and-ratio mechanism's own ceiling sits, at a cold
//! start and across idle durations, entirely independent of the gate
//! below - both layers are load-bearing, not alternatives to each other.
//!
//! # `ToneDominance`: energy is not enough, it has to be at the right frequencies
//!
//! [`ToneDominance`] is the fix: a Goertzel pair tuned to this end's
//! *listening* mark and space frequencies (see [`crate::Role::listen`] -
//! getting transmit and listen backwards here is exactly the bug Task 12
//! had to fix once already), run over fixed-length blocks alongside a
//! plain sum-of-squares of the same block's raw samples. A block is
//! judged tone-dominant only when the two Goertzel bins' combined power
//! exceeds the block's own total power by [`DOMINANCE_RATIO`] - i.e. when
//! very nearly all of the block's energy sits at exactly the two
//! frequencies a real Bell 103 signal would occupy, not merely somewhere
//! in-band. `Rx` (see its own doc) feeds the *previous* block's verdict
//! forward to gate the *current* block's energy before it ever reaches
//! [`CarrierDetector::update`]: zero when not dominant, the correlator's
//! real measured energy unchanged when it is. `CarrierDetector` itself -
//! every constant and every test above and below this section - is
//! completely unaware this gate exists; it only ever sees a number called
//! `energy`, and the fix is entirely about what that number is allowed to
//! be, not about how this module decides what to do with it. That is
//! deliberate: the floor-and-hysteresis machinery is proven independently
//! (this module's own tests, none of which change here), and a broadband
//! click gated to zero energy is, as far as `update` can tell, indistinguishable
//! from a quiet line - which is exactly what it should look like.
//!
//! This works because a real tone and broadband noise/a click behave
//! oppositely under a Goertzel bank as the block gets longer: a Bell 103
//! tone's dominance ratio grows linearly with block length (the matched
//! filter's coherent gain), amplitude-independent, while white noise's
//! dominance ratio has a distribution that does *not* narrow with a
//! longer block - a single frequency bin's power from white noise input
//! is not a consistent estimator, so more samples buys the tone more
//! separation without buying the noise any more precision. Measured
//! directly at [`DOMINANCE_BLOCK`] = 256 samples (32 ms at `DSP_RATE`):
//!
//! | Signal | Dominance ratio |
//! |---|---|
//! | Real Bell 103 tone (either band, any amplitude - ratio is scale-invariant) | ~127.9 |
//! | This crate's off-hook click, worst phase alignment over its whole 50 ms | ~5.2 |
//! | White noise, any RMS, worst of 2,000,000 independent blocks | ~17.7 (P(>20) not observed in 2M trials) |
//! | A full-amplitude tone in the *other* Bell 103 band | ~0.06 |
//! | ANSam (2100 Hz) against the originate-listening band it sits nearest | ~2.0 |
//!
//! [`DOMINANCE_RATIO`] = 40 sits with real margin on both sides: about
//! 3.2x below a genuine tone, about 2x above the worst broadband case
//! measured. See that constant's own doc for the reasoning and the
//! exploration this table was measured with.
//!
//! Gating on the *previous* block, not the current one, costs up to
//! about two block lengths (64 ms) of extra latency before a genuine
//! carrier's energy starts reaching `update` at all - real, but small
//! next to `HOLDOFF_SECONDS` and every acquisition preamble this crate's
//! tests already budget for (hundreds of ms). The alternative - judging
//! and gating the same block - would need buffering and replaying that
//! block's samples after the fact, delaying the deframer's own carrier
//! gate by a full block on every rise, not just the first; the one-block
//! lag this module uses instead only ever costs an extra block on
//! *acquisition*, never on ordinary operation once a real signal is
//! already up (see `Rx::push_sample`'s own doc for exactly where the
//! gate is applied).
//!
//! The correlators do already provide real frequency selectivity, which is
//! why any of this works at all: a matched tone accumulates coherently
//! across the correlator's window while broadband noise accumulates
//! incoherently, so equal-amplitude tone and noise are not read as equal
//! energy. Measured directly for this correlator's 27-sample window: a
//! pure tone reads about 7.1x louder than noise of the same amplitude on
//! a single band, close to the sqrt(27) ~ 5.2x an incoherent-sum argument
//! predicts. The *energy* metric this detector actually uses sums both
//! bands, which roughly halves that margin for noise specifically (noise
//! excites both bands independently; a clean tone excites essentially
//! only one) - measured at about 3.55x for equal-amplitude tone versus
//! noise through the real `(m + s) / win` calculation. That discrimination
//! is what makes a bounded floor useful instead of merely safe: without
//! it, no fixed ratio threshold would ever separate a weak signal from
//! noise loud enough to match its amplitude.

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
/// and hold that lock for as long as the noise itself continued. Splitting
/// the two states removes the trade-off:
/// FLOOR_ADAPT only has to be fast enough to be useful, because freezing -
/// not slowness - is what protects it once carrier is up.
const FLOOR_ADAPT: f64 = 1e-4;
/// Starting guess for the noise floor before any samples have calibrated
/// it, and the effective ceiling on how loud an ambient a cold start can
/// reject: energy above `RISE_RATIO * INITIAL_FLOOR` clears the threshold
/// in the first attack time constant, before FLOOR_ADAPT has moved this at
/// all, and then freezes there the moment it does. Measured against this
/// crate's noise sweep (`rx.rs`'s `noise_on_a_dead_line_produces_no_bytes`)
/// across 8 seeds and both Bell 103 bands: the boundary is not one precise
/// number, it is a **range**, about 0.0175 to 0.0205, and it does not
/// depend on which band the correlator is tuned to - individual seeds land
/// anywhere across that range on either band. An earlier version of this
/// comment quoted a single fixed-seed pair ("passes at 0.0205, fails at
/// 0.021") as though it were exact; that draw sat at the favourable end of
/// the same range, not a tighter true boundary - see `noise_on_a_dead_
/// line_produces_no_bytes`'s own doc for the sweep this was found with,
/// and `acc5372` for the same seed-lottery shape found once already in
/// this crate. A cold start cannot reliably reject anything above about
/// 0.0175; see the module doc for why, and `FLOOR_MIN_FRACTION` for the
/// equivalent figure once the line has been idle for a while (that figure
/// has not been re-checked for the same band-independence and may carry
/// the same caveat).
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
/// Lower bound on the floor, as a fraction of INITIAL_FLOOR.
///
/// FLOOR_ADAPT has no bottom of its own: while undetected it tracks
/// whatever it is fed, including genuine near-silence, and a few seconds
/// is enough for it to decay a long way down (see `update`'s comment).
/// Once it has, ordinary ambient noise far below any documented ceiling
/// reads as a large ratio spike and locks in as carrier, frozen, for as
/// long as that noise continues.
///
/// Measured, not picked, after 10 s of prior quiet (the boundary is stable
/// from there on - confirmed unchanged at 90 s), noise ceiling and
/// clean-signal sensitivity, both amplitude:
///
/// | fraction | floor min | noise ceiling | sensitivity |
/// |---|---|---|---|
/// | 1/3   | 3.3e-4 | 0.005 - 0.007   | 0.001 - 0.003    |
/// | 1/10  | 1e-4   | 0.002 - 0.0025  | 0.0005 - 0.0007  |
/// | 1/100 | 1e-5   | 0.0002 - 0.0003 | 0.00001 - 0.0001 |
/// | 1/1000| 1e-6   | < 0.0001        | 0.000001 - 0.00001 |
///
/// The requirement is rejecting this crate's 1e-3 noise reference after
/// any idle duration. Only 1/3 and 1/10 clear it - 1/100 and 1/1000 both
/// fail open well below 1e-3. 1/10 is the smaller of the two, so the most
/// downward adaptation - and hence the best sensitivity - available
/// without giving up that requirement: it holds 1e-3 with roughly 2x
/// margin against its measured 0.002-0.0025 ceiling.
const FLOOR_MIN_FRACTION: f64 = 1.0 / 10.0;

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
            let floor_min = INITIAL_FLOOR * FLOOR_MIN_FRACTION;
            if self.floor < floor_min {
                self.floor = floor_min;
            }
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

/// Samples per [`ToneDominance`] analysis block - 32 ms at `DSP_RATE`.
///
/// Chosen, not merely picked: a real Bell 103 tone's dominance ratio
/// grows linearly with block length (see this module's own doc), so a
/// longer block only ever widens the margin against noise, whose own
/// ratio distribution does not narrow with length at all. The bound the
/// other way is latency - gating on the *previous* block (see `Rx::
/// push_sample`'s own doc) costs up to two block lengths before a
/// genuine carrier's energy starts reaching [`CarrierDetector::update`]
/// at acquisition. 256 was measured comfortable on both sides: real
/// tone ratio ~127.9, worst white-noise draw ~17.7 over 2,000,000
/// independent blocks (see [`DOMINANCE_RATIO`]'s own doc for the full
/// table), while 64 ms worst-case extra latency is small next to
/// `HOLDOFF_SECONDS` and every acquisition preamble this crate's tests
/// already budget hundreds of ms for.
const DOMINANCE_BLOCK: usize = 256;

/// A block is tone-dominant when its two Goertzel bins' combined power
/// exceeds its own total (broadband) power by at least this factor.
///
/// Measured at [`DOMINANCE_BLOCK`] = 256 samples: a genuine Bell 103
/// tone reads about 127.9, scale-invariant. Against that, the worst
/// measured broadband cases: this crate's off-hook click peaks at about
/// 5.2 over its full 50 ms at any block-phase alignment, and white
/// noise's worst draw over 2,000,000 independent blocks reached about
/// 17.7. 40 sits with real margin on both sides.
const DOMINANCE_RATIO: f64 = 40.0;

/// Fractional frequency offsets probed either side of each nominal tone.
///
/// A single Goertzel at the exact nominal frequency is unusable here,
/// and the measurement is unambiguous. A pure mark tone reads 127.9 at
/// no offset, 72.7 at 1% and **6.2 at 2%** - below white noise - because
/// a 256-sample bin at `DSP_RATE` is about 31 Hz wide and 2% of 1270 Hz
/// is 25 Hz. This crate's own impairment suite models clock drift to
/// plus or minus 2%, so an exact-frequency test rejects exactly the
/// signals it exists to accept.
///
/// Probing the same span instead and keeping the best bin per tone
/// (see `feed`) holds dominance flat across the whole range: 127.9 at
/// no offset, 128.4 at plus 2%, 128.1 at minus 2%, 111.8 even at 2.5%.
const PROBE_OFFSETS: [f64; 5] = [-0.02, -0.01, 0.0, 0.01, 0.02];

/// The strongest probe in a set. Never empty, so the fold's identity is
/// never returned.
fn best(probes: &[crate::nco::Goertzel]) -> f64 {
    probes.iter().map(|g| g.power()).fold(0.0, f64::max)
}

/// Tells a genuine Bell 103 tone in this end's listening band from
/// broadband energy of the same or greater loudness - a click, ambient
/// noise, a burst of static - which is exactly what raw energy against
/// an adaptive floor cannot do on its own (see this module's own doc).
///
/// Built from this end's *listening* mark and space frequencies -
/// `tones(cfg.role.listen())`, the same pair `Rx`'s own demodulating
/// correlator uses, never `tones(cfg.role)` (this end's own *transmit*
/// band - see [`crate::Role::listen`]'s own doc for the bug that shape
/// of mistake has already shipped once in this crate). [`Rx::new`]
/// constructs both from the identical call, so there is exactly one
/// place either could drift from the other.
///
/// [`Rx::new`]: crate::rx::Rx::new
pub struct ToneDominance {
    mark: [crate::nco::Goertzel; PROBE_OFFSETS.len()],
    space: [crate::nco::Goertzel; PROBE_OFFSETS.len()],
    wideband: f64,
    count: usize,
}

impl ToneDominance {
    pub fn new(mark_hz: f64, space_hz: f64, sample_rate: f64) -> Self {
        let probe =
            |hz: f64| PROBE_OFFSETS.map(|o| crate::nco::Goertzel::new(hz * (1.0 + o), sample_rate));
        Self {
            mark: probe(mark_hz),
            space: probe(space_hz),
            wideband: 0.0,
            count: 0,
        }
    }

    /// Discards the part-judged block. Called when the line goes quiet:
    /// a block half-filled with a signal that has since stopped would
    /// otherwise be completed by whatever arrives next and judged as one
    /// thing, which is how a fragment of real tone could qualify a
    /// following burst of noise.
    pub fn reset(&mut self) {
        for g in self.mark.iter_mut().chain(self.space.iter_mut()) {
            g.reset();
        }
        self.wideband = 0.0;
        self.count = 0;
    }

    pub fn feed(&mut self, x: f64) -> Option<bool> {
        for g in self.mark.iter_mut().chain(self.space.iter_mut()) {
            g.feed(x);
        }
        self.wideband += x * x;
        self.count += 1;

        if self.count < DOMINANCE_BLOCK {
            return None;
        }

        // Best probe per tone, not the sum of all of them: a real tone
        // lands in one probe and the rest contribute nothing but noise,
        // so summing would raise the floor for every draw while adding
        // nothing to the signal. Max keeps the tone's full power and
        // leaves broadband energy roughly where it was - measured worst
        // white-noise draw over 50,000 blocks is 16.7 against the single
        // probe's 17.7.
        let narrow = best(&self.mark) + best(&self.space);
        // No epsilon needed: on true silence every sample is exactly
        // zero, so `narrow` is exactly zero too and this correctly reads
        // as not dominant (0 > 40*0 is false) rather than needing a
        // guard against dividing by a wideband of zero - there is no
        // division here at all.
        let dominant = narrow > DOMINANCE_RATIO * self.wideband;

        self.reset();
        Some(dominant)
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

    // --- ToneDominance -----------------------------------------------------

    fn sine_block(freq: f64, amplitude: f64, n: usize, sample_rate: f64) -> alloc::vec::Vec<f64> {
        let mut nco = crate::nco::Nco::new(freq, sample_rate);
        (0..n).map(|_| amplitude * nco.next()).collect()
    }

    /// Deterministic broadband noise, matching `overture.rs`'s own
    /// off-hook click generator's xorshift64 (see that module's
    /// `click_sample`) so this test can reason about the same shape of
    /// signal without depending on `overture` (a `no_std` layering
    /// concern - `carrier` is beneath `overture` in this crate, not
    /// beside it).
    fn noise_block(amplitude: f64, n: usize, seed: u64) -> alloc::vec::Vec<f64> {
        let mut state = seed;
        (0..n)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                (((state >> 40) as f64 / 8_388_608.0) - 1.0) * amplitude
            })
            .collect()
    }

    const FS: f64 = crate::DSP_RATE;
    const MARK: f64 = 1270.0;
    const SPACE: f64 = 1070.0;

    /// The property this whole gate exists for: a real Bell 103 tone in
    /// the listened-for band is judged dominant. Two full blocks so the
    /// verdict is read from a genuinely steady state, not the first one
    /// (which for a `Goertzel` starting from all-zero accumulators is a
    /// transient - see `Goertzel::feed`'s own doc).
    #[test]
    fn a_real_tone_in_the_listened_for_band_is_dominant() {
        let mut td = ToneDominance::new(MARK, SPACE, FS);
        let block = sine_block(MARK, 1.0, 2 * DOMINANCE_BLOCK, FS);

        // No verdict until exactly one full block has been fed.
        for &x in &block[..DOMINANCE_BLOCK - 1] {
            assert_eq!(
                td.feed(x),
                None,
                "returned a verdict before a full block was fed"
            );
        }
        assert!(
            td.feed(block[DOMINANCE_BLOCK - 1]).is_some(),
            "no verdict after exactly one full block"
        );

        // The verdict itself, read from the second block - a clean
        // steady state, not the first block's own start-up transient.
        let mut verdict = None;
        for &x in &block[DOMINANCE_BLOCK..] {
            verdict = td.feed(x);
        }
        assert_eq!(
            verdict,
            Some(true),
            "a full-amplitude in-band tone was not judged dominant"
        );
    }

    /// Scale invariance: dominance is a ratio of two quantities that both
    /// scale with amplitude squared, so a much quieter tone must be
    /// judged exactly as dominant as a full-amplitude one - this is what
    /// lets the gate stay out of the way of `CarrierDetector`'s own,
    /// separate sensitivity floor (see this module's own doc).
    #[test]
    fn dominance_is_scale_invariant_for_a_real_tone() {
        let block = sine_block(MARK, 0.0075, 2 * DOMINANCE_BLOCK, FS);
        let mut td = ToneDominance::new(MARK, SPACE, FS);
        let mut verdict = None;
        for &x in &block {
            verdict = td.feed(x);
        }
        assert_eq!(
            verdict,
            Some(true),
            "a quiet (0.0075 amplitude) in-band tone was not judged dominant"
        );
    }

    /// A full-amplitude tone in the *other* Bell 103 band must not be
    /// judged dominant against this filter's own band - the property the
    /// whole split exists to prove (see `lib.rs`'s `Role` doc).
    #[test]
    fn a_tone_in_the_other_band_is_not_dominant() {
        let mut td = ToneDominance::new(MARK, SPACE, FS);
        let block = sine_block(2225.0, 1.0, 2 * DOMINANCE_BLOCK, FS);
        let mut verdict = None;
        for &x in &block {
            verdict = td.feed(x);
        }
        assert_eq!(
            verdict,
            Some(false),
            "a wrong-band tone was judged dominant"
        );
    }

    /// This crate's own off-hook click (see `overture.rs`'s
    /// `click_sample` - reproduced here rather than imported, see
    /// `noise_block`'s own doc) must never be judged dominant, at every
    /// block-aligned phase across its whole decay. This is the
    /// regression this task exists to fix.
    #[test]
    fn the_off_hook_click_is_never_dominant() {
        // OFF_HOOK_AMPLITUDE (0.4) and OFF_HOOK_TAU_S (0.008s) from
        // overture.rs, padded well past its 50 ms duration so every
        // block-aligned window sees the full decay and the silence after
        // it.
        let seed = 0x2545F491_4F6CDD1Du64;
        let mut state = seed;
        let n = 2000usize;
        let click: alloc::vec::Vec<f64> = (0..n)
            .map(|i| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                let noise = ((state >> 40) as f64 / 8_388_608.0) - 1.0;
                let t = i as f64 / FS;
                if t < 0.05 {
                    noise * 0.4 * libm::exp(-t / 0.008)
                } else {
                    0.0
                }
            })
            .collect();

        let mut td = ToneDominance::new(MARK, SPACE, FS);
        for chunk in click.chunks(DOMINANCE_BLOCK) {
            if chunk.len() < DOMINANCE_BLOCK {
                break;
            }
            for &x in chunk {
                if let Some(dominant) = td.feed(x) {
                    assert!(
                        !dominant,
                        "the off-hook click was judged dominant in some block"
                    );
                }
            }
        }
    }

    /// Broadband noise at the *same* amplitude a real detected carrier
    /// would use (this crate's `Tx` transmits at amplitude 1.0 - see
    /// `tx.rs`) must not be judged dominant. Dominance is scale-invariant
    /// for noise as for a tone (see this module's own doc), so this also
    /// stands in for any louder noise.
    #[test]
    fn broadband_noise_as_loud_as_a_real_carrier_is_not_dominant() {
        let mut td = ToneDominance::new(MARK, SPACE, FS);
        let noise = noise_block(1.0, 20 * DOMINANCE_BLOCK, 0xF00D_F00D_F00D_F00D);
        for x in noise {
            if let Some(dominant) = td.feed(x) {
                assert!(
                    !dominant,
                    "broadband noise at amplitude 1.0 was judged dominant"
                );
            }
        }
    }

    /// Mutation-proven: swapping `ToneDominance::new`'s two arguments at a
    /// call site is exactly the transmit/listen mix-up `lib.rs`'s `Role`
    /// doc already warns about, at a different call site. Built here from
    /// `tones(Role::Originate)` (1270/1070) while fed a real Answer-band
    /// (2225/2025) tone - the shape a receiver listening on its own
    /// transmit band instead of the far end's would actually see - and
    /// the verdict must still correctly read "not dominant", proving the
    /// two arguments are not merely interchangeable labels for the same
    /// pair.
    #[test]
    fn mark_and_space_are_not_interchangeable() {
        let mut td = ToneDominance::new(1270.0, 1070.0, FS);
        let block = sine_block(2225.0, 1.0, 2 * DOMINANCE_BLOCK, FS);
        let mut verdict = None;
        for &x in &block {
            verdict = td.feed(x);
        }
        assert_eq!(verdict, Some(false));
    }
}
