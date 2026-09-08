//! The overture: the performed dial-up handshake.
//!
//! This is the sound people remember, rendered as real signals at real
//! timings, not a recording. Every stage down to [`Stage::Cj`] is a
//! genuine rendering of what that stage sounds like; [`Stage::Training`]
//! is explicitly an *impression* of V.34-style probing, not a conformant
//! one, and this module says so rather than pretending otherwise.
//!
//! # Dialling is decorative
//!
//! There is no phone network here. `Overture::new("01234")` performs
//! those DTMF digits and nothing more - the listening end (Task 12's
//! `Session`) always answers, regardless of what digits were dialled.
//! `Stage::Ringback` always follows, and CI always follows that. Nothing
//! reads the dialled digits back.
//!
//! # Why this does not take a `Config` or a `Role`
//!
//! `Role` (see `lib.rs`) selects a Bell 103 band, and Task 12 documents
//! that its meaning is already ambiguous between the band a `Tx`
//! transmits and the band an `Rx` listens on. The overture is transmit
//! only and touches neither Bell 103 band nor that ambiguity: `Stage::Ci`,
//! `Stage::CmJm` and `Stage::Cj` are V.21 - 980/1180 Hz, entirely
//! different tones from Bell 103's 1270/1070/2225/2025 Hz - and every
//! other stage is a dial tone, DTMF, a ringback cadence or ANSam, none of
//! which involve a Bell 103 band either. Taking a `Role` parameter here
//! would invite exactly the confusion Task 12 has to resolve; `Overture`
//! takes none, on purpose.
//!
//! # Why this does not reuse `Tx`
//!
//! `Tx` (see `tx.rs`) only ever tunes to the Bell 103 pair a `Role`
//! selects, and has no way to be pointed at V.21's 980/1180 Hz pair - so
//! it cannot render CI, CM/JM or CJ regardless of how it is driven. This
//! module has its own small bit modulator (see [`FskChannel`]) instead.
//!
//! `Tx` also has a one-symbol lead-in: it idles on mark and only retunes
//! at a symbol boundary *after* that boundary's sample has already been
//! emitted, so the first symbol period of any `Tx` transmission is always
//! idle mark rather than the first queued bit (see `tx.rs`'s
//! `samples_for_bits` doc). Stacking several `Tx`-driven stages back to
//! back by raw sample count loses each stage's last bit into the next
//! one's opening samples. [`FskChannel`] does not have this defect: it
//! sets the first bit's tone before generating that bit's first sample,
//! so a stage built from it renders exactly its queued bits, in exactly
//! that many symbol periods, with nothing lost at the seam. This is a
//! deliberate difference from `Tx`, not an oversight - see
//! [`FskChannel::new`].
//!
//! # Training must contain transitions
//!
//! `rx.rs`'s Gardner timing loop cannot acquire lock on a tone that never
//! changes - it measures transitions, and a constant tone has none. A
//! `Stage::Training` of steady tones would sound entirely plausible and
//! leave a real receiver's timing loop unlocked. This module renders
//! training as an alternating `0x55` bit pattern over V.21's low channel
//! for exactly that reason - see [`TRAINING_PAYLOAD`] and this module's
//! `training_stage_contains_transitions_not_a_steady_tone` test, which is
//! the test that would have caught shipping a steady tone here (see the
//! task report's Mutation 4).
//!
//! # Timings
//!
//! | Stage | Rendered as |
//! |---|---|
//! | Off hook | A short decaying broadband click - the audible artefact of the relay closing, not a tone |
//! | Dial tone | UK: 350 + 450 Hz, continuous, for [`DIAL_TONE_S`] |
//! | Dialling | DTMF, 100 ms tone / 100 ms gap per digit |
//! | Ringback | UK double ring: 400 + 450 Hz, 0.4 s on / 0.2 s off / 0.4 s on / 2.0 s silent, one cycle |
//! | CI | "CI", V.21 low channel, 300 bit/s |
//! | ANSam | 2100 Hz, phase reversals every 450 ms, 15 Hz amplitude modulation, [`ANSAM_S`] |
//! | CM/JM | "CMJM", V.21 low channel, 300 bit/s |
//! | CJ | "CJ" acknowledgement, then a 75 ms silent transition |
//! | Training | An impression: alternating `0x55` over V.21 low channel, not conformant V.34 probing |
//!
//! `rate` is expected to stay constant across the calls that render one
//! `Overture`, the same convention `Tx` and `Rx` use for `Config`'s
//! `sample_rate` - each internal oscillator is built once, at whatever
//! `rate` was current when its stage began, and is not rebuilt mid-stage.

use alloc::collections::VecDeque;
use alloc::vec::Vec;
use libm::{exp, floor};

use crate::frame::frame_byte;
use crate::nco::Nco;
use crate::BAUD;

/// V.21's low channel. Distinct from both Bell 103 bands (1270/1070 and
/// 2225/2025 - see `lib.rs::tones`) and from V.21's own high channel
/// (1650/1850), which this module never uses.
const V21_LOW_MARK: f64 = 980.0;
const V21_LOW_SPACE: f64 = 1180.0;

/// UK dial tone. The US pair is 350 + 440 - see this module's
/// `uk_dial_tone_is_350_450_not_the_us_pair` test and Mutation 1 in the
/// task report.
const DIAL_TONE_A: f64 = 350.0;
const DIAL_TONE_B: f64 = 450.0;
/// Real dial tone runs indefinitely until dialling starts; this is a
/// performance, so it runs for a fixed, short stretch instead. Chosen,
/// not measured against anything external.
const DIAL_TONE_S: f64 = 1.0;

const DTMF_TONE_S: f64 = 0.1;
const DTMF_GAP_S: f64 = 0.1;
const DIGIT_PERIOD_S: f64 = DTMF_TONE_S + DTMF_GAP_S;

/// UK ringback.
const RING_A: f64 = 400.0;
const RING_B: f64 = 450.0;
const RING_ON1_S: f64 = 0.4;
const RING_OFF_S: f64 = 0.2;
const RING_ON2_S: f64 = 0.4;
const RING_SILENT_S: f64 = 2.0;
/// One double-ring cycle only - the far end always answers (dialling is
/// decorative, see this module's doc), so there is no reason to perform a
/// second one.
const RING_CYCLE_S: f64 = RING_ON1_S + RING_OFF_S + RING_ON2_S + RING_SILENT_S;

const ANSAM_FREQ: f64 = 2100.0;
/// Per the brief: phase reversals every 450 ms.
const ANSAM_REVERSAL_S: f64 = 0.45;
const ANSAM_AM_FREQ: f64 = 15.0;
/// Modulation depth. Chosen so the envelope (`1.0 +/- ANSAM_AM_DEPTH`)
/// never reaches zero - a null would make "is the 15 Hz AM present"
/// harder to measure cleanly and buys nothing real here.
const ANSAM_AM_DEPTH: f64 = 0.3;
const ANSAM_SCALE: f64 = 0.7;
/// "About 3 s" per the brief.
const ANSAM_S: f64 = 3.0;

const CI_PAYLOAD: &[u8] = b"CICICI";
const CMJM_PAYLOAD: &[u8] = b"CMJMCMJM";
const CJ_PAYLOAD: &[u8] = b"CJCJCJ";
/// The transition silence after CJ's acknowledgement, before training
/// starts.
const CJ_SILENCE_S: f64 = 0.075;

/// 30 bytes of alternating `0x55` at 300 bit/s is exactly 1.0 s (300
/// bits). An impression of V.34 probing, not conformant - see this
/// module's doc.
const TRAINING_PAYLOAD: [u8; 30] = [0x55u8; 30];

/// Two simultaneous tones (dial tone, DTMF, ringback), each at this
/// amplitude, so the sum never exceeds 1.0 and clips.
const TWO_TONE_AMPLITUDE: f64 = 0.45;

const OFF_HOOK_S: f64 = 0.05;
/// Decay time constant for the relay click - short enough that almost
/// all its energy sits in the first ~15 ms.
const OFF_HOOK_TAU_S: f64 = 0.008;
const OFF_HOOK_AMPLITUDE: f64 = 0.4;
/// Arbitrary fixed seed. The click is a deterministic performance, not
/// genuine entropy.
const OFF_HOOK_SEED: u64 = 0x2545F491_4F6CDD1D;

/// One stage of the performed handshake, in the order the overture
/// renders them. [`Overture::read`] reports which stage owns the samples
/// it just produced.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Stage {
    OffHook,
    DialTone,
    Dialling,
    Ringback,
    Ci,
    Ansam,
    CmJm,
    Cj,
    Training,
    Connected,
}

impl Stage {
    pub fn name(&self) -> &'static str {
        match self {
            Stage::OffHook => "Off hook",
            Stage::DialTone => "Dial tone",
            Stage::Dialling => "Dialling",
            Stage::Ringback => "Ringback",
            Stage::Ci => "CI",
            Stage::Ansam => "ANSam",
            Stage::CmJm => "CM/JM",
            Stage::Cj => "CJ",
            Stage::Training => "Training",
            Stage::Connected => "Connected",
        }
    }

    fn next(self) -> Stage {
        match self {
            Stage::OffHook => Stage::DialTone,
            Stage::DialTone => Stage::Dialling,
            Stage::Dialling => Stage::Ringback,
            Stage::Ringback => Stage::Ci,
            Stage::Ci => Stage::Ansam,
            Stage::Ansam => Stage::CmJm,
            Stage::CmJm => Stage::Cj,
            Stage::Cj => Stage::Training,
            Stage::Training => Stage::Connected,
            Stage::Connected => Stage::Connected,
        }
    }
}

/// Standard 4x4 DTMF grid. `None` for anything outside `0-9`, `*`, `#`
/// and `A-D`.
fn dtmf_freqs(b: u8) -> Option<(f64, f64)> {
    const ROWS: [f64; 4] = [697.0, 770.0, 852.0, 941.0];
    const COLS: [f64; 4] = [1209.0, 1336.0, 1477.0, 1633.0];
    let (row, col) = match b {
        b'1' => (0, 0),
        b'2' => (0, 1),
        b'3' => (0, 2),
        b'A' => (0, 3),
        b'4' => (1, 0),
        b'5' => (1, 1),
        b'6' => (1, 2),
        b'B' => (1, 3),
        b'7' => (2, 0),
        b'8' => (2, 1),
        b'9' => (2, 2),
        b'C' => (2, 3),
        b'*' => (3, 0),
        b'0' => (3, 1),
        b'#' => (3, 2),
        b'D' => (3, 3),
        _ => return None,
    };
    Some((ROWS[row], COLS[col]))
}

/// A short, deterministic broadband click with a fast exponential decay -
/// the audible artefact of a relay closing, not a tone. `t` is elapsed
/// seconds since the click started.
fn click_sample(rng: &mut u64, t: f64) -> f64 {
    *rng ^= *rng << 13;
    *rng ^= *rng >> 7;
    *rng ^= *rng << 17;
    let n = ((*rng >> 40) as f64 / 8_388_608.0) - 1.0; // roughly [-1, 1)
    n * OFF_HOOK_AMPLITUDE * exp(-t / OFF_HOOK_TAU_S)
}

/// Which digit (by index into the dialled digits) and tone/gap sub-phase
/// `t` seconds into `Stage::Dialling` falls in.
fn dialling_phase(t: f64) -> (usize, bool) {
    let digit_i = floor(t / DIGIT_PERIOD_S) as usize;
    let phase_t = t - digit_i as f64 * DIGIT_PERIOD_S;
    (digit_i, phase_t < DTMF_TONE_S)
}

/// Whether the UK ringback cadence is sounding (as opposed to silent) at
/// `t` seconds into `Stage::Ringback`.
fn ringback_gate(t: f64) -> bool {
    let off_at = RING_ON1_S;
    let on2_at = off_at + RING_OFF_S;
    let silent_at = on2_at + RING_ON2_S;
    t < off_at || (t >= on2_at && t < silent_at)
}

/// A minimal FSK bit modulator for V.21's low channel, used by
/// [`Stage::Ci`], [`Stage::CmJm`], [`Stage::Cj`] and [`Stage::Training`].
///
/// Unlike `Tx` (see this module's doc), the first queued bit's tone is
/// set *before* that bit's first sample is generated, so there is no
/// idle-mark lead-in: `n` queued bits occupy exactly `n` symbol periods,
/// and [`FskChannel::finished`] becomes true the instant the last of them
/// has been fully rendered - not one symbol early and not one late. That
/// is what lets [`Overture`] stack stages back to back by "render this
/// channel until it reports finished" rather than by a raw sample count,
/// sidestepping the stacking pitfall `tx.rs`'s `samples_for_bits` names.
struct FskChannel {
    nco: Nco,
    mark: f64,
    space: f64,
    bits: VecDeque<bool>,
    total_bits: usize,
    symbols_done: usize,
    sym_phase: f64,
    sym_step: f64,
}

impl FskChannel {
    fn new(mark: f64, space: f64, rate: u32, payload: &[u8]) -> Self {
        let mut bits = VecDeque::new();
        for &b in payload {
            bits.extend(frame_byte(b));
        }
        let total_bits = bits.len();
        let first = bits.pop_front().unwrap_or(true);
        let nco = Nco::new(if first { mark } else { space }, rate as f64);
        Self {
            nco,
            mark,
            space,
            bits,
            total_bits,
            symbols_done: 0,
            sym_phase: 0.0,
            sym_step: BAUD / rate as f64,
        }
    }

    fn finished(&self) -> bool {
        self.symbols_done >= self.total_bits
    }

    fn next_sample(&mut self) -> f64 {
        let s = self.nco.next();
        self.sym_phase += self.sym_step;
        if self.sym_phase >= 1.0 {
            self.sym_phase -= 1.0;
            self.symbols_done += 1;
            if let Some(bit) = self.bits.pop_front() {
                self.nco.set_freq(if bit { self.mark } else { self.space });
            }
        }
        s
    }
}

enum StageState {
    OffHook {
        rng: u64,
        n: u64,
    },
    DialTone {
        a: Nco,
        b: Nco,
        n: u64,
    },
    Dialling {
        digit_i: usize,
        in_tone: bool,
        a: Nco,
        b: Nco,
        n: u64,
    },
    Ringback {
        a: Nco,
        b: Nco,
        n: u64,
    },
    Ci(FskChannel),
    Ansam {
        carrier: Nco,
        env: Nco,
        n: u64,
    },
    CmJm(FskChannel),
    Cj {
        fsk: FskChannel,
        silence_n: u64,
    },
    Training(FskChannel),
    Connected,
}

/// How many samples `dur_s` seconds occupies at `rate` samples per
/// second. Stage completion is decided by comparing an exact sample
/// count against this, never by accumulating `1.0 / rate` once per
/// sample - that accumulation's rounding error, though tiny per step,
/// compounds over thousands of samples and was measured to shift a
/// boundary by enough to leak a sample or two of tone into what a test
/// expected to be exact silence.
fn stage_samples_for(dur_s: f64, rate: u32) -> u64 {
    libm::round(dur_s * rate as f64) as u64
}

/// The performed dial-up handshake. See this module's doc for the full
/// design and the timings table.
pub struct Overture {
    digits: Vec<u8>,
    stage: Stage,
    state: StageState,
}

impl Overture {
    /// `digits` is whatever followed `ATDT` - anything outside the
    /// standard DTMF grid (`0-9`, `*`, `#`, `A-D`) is silently dropped.
    /// Dialling is decorative (see this module's doc): nothing reads
    /// these digits back, they are only performed as DTMF tones.
    pub fn new(digits: &str) -> Self {
        let digits: Vec<u8> = digits
            .bytes()
            .map(|b| b.to_ascii_uppercase())
            .filter(|&b| dtmf_freqs(b).is_some())
            .collect();
        Self {
            digits,
            stage: Stage::OffHook,
            state: StageState::OffHook {
                rng: OFF_HOOK_SEED,
                n: 0,
            },
        }
    }

    /// Fills `out` with the next block of the performance at `rate`
    /// samples per second, holding silence once [`Stage::Connected`] is
    /// reached. `rate` is expected constant across the calls that render
    /// one `Overture` - see this module's doc.
    ///
    /// Returns how many samples were written (always `out.len()`), the
    /// stage that owns the last sample written, and whether that stage is
    /// `Stage::Connected` - the overture is complete once this is `true`.
    ///
    /// The owning stage is captured *before* generating each sample, not
    /// read back from `self.stage` afterwards: `next_sample` can advance
    /// `self.stage` mid-call, the instant the sample that completes a
    /// stage is produced, so reading `self.stage` only after the loop
    /// would label that exact sample with the stage that comes *next*
    /// instead of the one that actually rendered it - a one-sample
    /// mislabelling at every stage boundary. Caught by this module's own
    /// tests: `render_all` reads one sample at a time specifically to
    /// make a boundary error like that visible, and it shifted every
    /// stage's measured content by one sample before this fix.
    pub fn read(&mut self, out: &mut [f32], rate: u32) -> (usize, Stage, bool) {
        assert!(rate > 0, "rate must be greater than zero");
        let mut owner = self.stage;
        for slot in out.iter_mut() {
            owner = self.stage;
            *slot = if self.stage == Stage::Connected {
                0.0
            } else {
                self.next_sample(rate) as f32
            };
        }
        (out.len(), owner, owner == Stage::Connected)
    }

    fn next_sample(&mut self, rate: u32) -> f64 {
        let (sample, finished) = self.generate(rate);
        if finished {
            self.stage = self.stage.next();
            self.state = Self::enter_state(self.stage, &self.digits, rate);
        }
        sample
    }

    /// Produces the current stage's next sample and reports whether it
    /// was that stage's last one.
    fn generate(&mut self, rate: u32) -> (f64, bool) {
        let digits = self.digits.as_slice();
        match &mut self.state {
            StageState::OffHook { rng, n } => {
                let t = *n as f64 / rate as f64;
                let sample = click_sample(rng, t);
                *n += 1;
                (sample, *n >= stage_samples_for(OFF_HOOK_S, rate))
            }
            StageState::DialTone { a, b, n } => {
                let sample = a.next() * TWO_TONE_AMPLITUDE + b.next() * TWO_TONE_AMPLITUDE;
                *n += 1;
                (sample, *n >= stage_samples_for(DIAL_TONE_S, rate))
            }
            StageState::Dialling {
                digit_i,
                in_tone,
                a,
                b,
                n,
            } => {
                if *digit_i >= digits.len() {
                    // No dialable digits at all: a single near-silent
                    // sample, then move straight on.
                    *n += 1;
                    return (0.0, true);
                }
                let sample = if *in_tone {
                    a.next() * TWO_TONE_AMPLITUDE + b.next() * TWO_TONE_AMPLITUDE
                } else {
                    0.0
                };
                *n += 1;
                let t = *n as f64 / rate as f64;
                let (new_digit_i, new_in_tone) = dialling_phase(t);
                let finished = new_digit_i >= digits.len();
                if !finished && (new_digit_i, new_in_tone) != (*digit_i, *in_tone) {
                    *digit_i = new_digit_i;
                    *in_tone = new_in_tone;
                    if new_in_tone {
                        let (fa, fb) = dtmf_freqs(digits[new_digit_i]).unwrap_or((1.0, 1.0));
                        *a = Nco::new(fa, rate as f64);
                        *b = Nco::new(fb, rate as f64);
                    }
                }
                (sample, finished)
            }
            StageState::Ringback { a, b, n } => {
                let t = *n as f64 / rate as f64;
                let sample = if ringback_gate(t) {
                    a.next() * TWO_TONE_AMPLITUDE + b.next() * TWO_TONE_AMPLITUDE
                } else {
                    0.0
                };
                *n += 1;
                (sample, *n >= stage_samples_for(RING_CYCLE_S, rate))
            }
            StageState::Ci(fsk) => {
                let sample = fsk.next_sample();
                (sample, fsk.finished())
            }
            StageState::Ansam { carrier, env, n } => {
                let t = *n as f64 / rate as f64;
                let half = floor(t / ANSAM_REVERSAL_S) as u64;
                let sign = if half.is_multiple_of(2) { 1.0 } else { -1.0 };
                let carrier_sample = carrier.next() * sign;
                let envelope = 1.0 + ANSAM_AM_DEPTH * env.next();
                let sample = carrier_sample * envelope * ANSAM_SCALE;
                *n += 1;
                (sample, *n >= stage_samples_for(ANSAM_S, rate))
            }
            StageState::CmJm(fsk) => {
                let sample = fsk.next_sample();
                (sample, fsk.finished())
            }
            StageState::Cj { fsk, silence_n } => {
                if !fsk.finished() {
                    (fsk.next_sample(), false)
                } else {
                    *silence_n += 1;
                    (0.0, *silence_n >= stage_samples_for(CJ_SILENCE_S, rate))
                }
            }
            StageState::Training(fsk) => {
                let sample = fsk.next_sample();
                (sample, fsk.finished())
            }
            StageState::Connected => (0.0, false),
        }
    }

    fn enter_state(stage: Stage, digits: &[u8], rate: u32) -> StageState {
        match stage {
            Stage::OffHook => StageState::OffHook {
                rng: OFF_HOOK_SEED,
                n: 0,
            },
            Stage::DialTone => StageState::DialTone {
                a: Nco::new(DIAL_TONE_A, rate as f64),
                b: Nco::new(DIAL_TONE_B, rate as f64),
                n: 0,
            },
            Stage::Dialling => {
                let (fa, fb) = digits
                    .first()
                    .and_then(|&d| dtmf_freqs(d))
                    .unwrap_or((1.0, 1.0));
                StageState::Dialling {
                    digit_i: 0,
                    in_tone: true,
                    a: Nco::new(fa, rate as f64),
                    b: Nco::new(fb, rate as f64),
                    n: 0,
                }
            }
            Stage::Ringback => StageState::Ringback {
                a: Nco::new(RING_A, rate as f64),
                b: Nco::new(RING_B, rate as f64),
                n: 0,
            },
            Stage::Ci => StageState::Ci(FskChannel::new(
                V21_LOW_MARK,
                V21_LOW_SPACE,
                rate,
                CI_PAYLOAD,
            )),
            Stage::Ansam => StageState::Ansam {
                carrier: Nco::new(ANSAM_FREQ, rate as f64),
                env: Nco::new(ANSAM_AM_FREQ, rate as f64),
                n: 0,
            },
            Stage::CmJm => StageState::CmJm(FskChannel::new(
                V21_LOW_MARK,
                V21_LOW_SPACE,
                rate,
                CMJM_PAYLOAD,
            )),
            Stage::Cj => StageState::Cj {
                fsk: FskChannel::new(V21_LOW_MARK, V21_LOW_SPACE, rate, CJ_PAYLOAD),
                silence_n: 0,
            },
            Stage::Training => StageState::Training(FskChannel::new(
                V21_LOW_MARK,
                V21_LOW_SPACE,
                rate,
                &TRAINING_PAYLOAD,
            )),
            Stage::Connected => StageState::Connected,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nco::goertzel;
    use alloc::vec;

    const RATE: u32 = 8000;

    /// Renders a whole `Overture` to completion at [`RATE`], grouping
    /// consecutive same-stage samples together in stage order. Reads one
    /// sample at a time so a stage boundary is never smeared across a
    /// group - every test in this file that needs to isolate a stage's
    /// audio, or measure a cadence within one, depends on that precision.
    ///
    /// Panics if the overture has not reached `Stage::Connected` within
    /// 30 s of rendered audio, so a runaway stage machine fails loudly
    /// rather than hanging the test suite.
    fn render_all(digits: &str) -> Vec<(Stage, Vec<f32>)> {
        let mut ov = Overture::new(digits);
        let mut groups: Vec<(Stage, Vec<f32>)> = Vec::new();
        let mut buf = [0.0f32; 1];
        loop {
            let (_, stage, done) = ov.read(&mut buf, RATE);
            match groups.last_mut() {
                Some((s, v)) if *s == stage => v.push(buf[0]),
                _ => groups.push((stage, vec![buf[0]])),
            }
            if done {
                break;
            }
            let total: usize = groups.iter().map(|(_, v)| v.len()).sum();
            assert!(
                total < RATE as usize * 30,
                "overture did not reach Connected within 30 s of audio"
            );
        }
        groups
    }

    fn stage_samples(groups: &[(Stage, Vec<f32>)], stage: Stage) -> &[f32] {
        &groups
            .iter()
            .find(|(s, _)| *s == stage)
            .unwrap_or_else(|| panic!("stage {stage:?} did not appear in the rendered overture"))
            .1
    }

    fn f64s(v: &[f32]) -> Vec<f64> {
        v.iter().map(|&x| x as f64).collect()
    }

    /// The trap named directly in the brief: a test that only checks 350
    /// Hz passes against the US dial tone (350 + 440) too. This checks
    /// the UK pair is present and 440 Hz specifically is not, using a
    /// one-second (8000-sample) window - at 8 kHz every frequency this
    /// module uses is an integer number of Hz, so a one-second window
    /// gives every one of them an exact integer cycle count and hence
    /// (by DFT orthogonality) zero leakage from the others into a
    /// Goertzel bin that is not actually present in the signal. See this
    /// module's doc and the task report for why that makes 1 s at 8 kHz
    /// the standard measurement window throughout this file.
    #[test]
    fn uk_dial_tone_is_350_450_not_the_us_pair() {
        let groups = render_all("1");
        let dial = stage_samples(&groups, Stage::DialTone);
        assert!(dial.len() >= RATE as usize, "dial tone shorter than 1 s");
        let win = f64s(&dial[..RATE as usize]);
        assert!(
            goertzel(&win, 350.0, RATE as f64) >= 0.35,
            "350 Hz not present in dial tone"
        );
        assert!(
            goertzel(&win, 450.0, RATE as f64) >= 0.35,
            "450 Hz (the UK pair's second tone) not present in dial tone"
        );
        assert!(
            goertzel(&win, 440.0, RATE as f64) <= 0.05,
            "440 Hz (the US pair's second tone) present in dial tone - this is the US pair, not the UK one"
        );
    }

    /// The trap named directly in the brief, the other half: a stage that
    /// emits the right tones *plus* other things also passes an
    /// only-checks-presence test. This asserts the dial tone contains
    /// nothing at the DTMF or ANSam frequencies either.
    #[test]
    fn dial_tone_contains_only_350_and_450() {
        let groups = render_all("1");
        let dial = stage_samples(&groups, Stage::DialTone);
        let win = f64s(&dial[..RATE as usize]);
        for freq in [
            697.0, 770.0, 852.0, 941.0, // DTMF rows
            1209.0, 1336.0, 1477.0, 1633.0, // DTMF columns
            2100.0, // ANSam
        ] {
            let mag = goertzel(&win, freq, RATE as f64);
            assert!(
                mag <= 0.05,
                "dial tone has unexpected energy at {freq} Hz: {mag}"
            );
        }
    }

    /// Digit '1' is 697 + 1209 Hz. A second digit with an entirely
    /// different pair ('5' is 770 + 1336 Hz) rules out a single hardcoded
    /// pair passing for every digit.
    ///
    /// The presence threshold is 0.25, not the 0.9-ish figure a pure,
    /// cleanly-windowed tone would read: unlike 350/450/400 Hz (all
    /// multiples of 10, so a 100 ms window is an exact number of
    /// cycles), most of the DTMF grid does not divide evenly into an
    /// 8 kHz, 100 ms window - 1336 Hz measures only 133.6 cycles in
    /// 800 samples - so some of each tone's energy leaks out of its own
    /// Goertzel bin. Measured directly: 0.34-0.45 for the four tones
    /// this test checks, comfortably clear of the near-zero (<= 0.01)
    /// an absent tone actually measures at, which is what the 0.25
    /// threshold is set relative to.
    #[test]
    fn dtmf_digits_map_to_the_standard_grid() {
        let groups = render_all("15");
        let dialling = stage_samples(&groups, Stage::Dialling);
        let tone_len = (DTMF_TONE_S * RATE as f64) as usize;

        let digit1 = f64s(&dialling[..tone_len]);
        assert!(
            goertzel(&digit1, 697.0, RATE as f64) >= 0.25,
            "digit 1: no 697 Hz"
        );
        assert!(
            goertzel(&digit1, 1209.0, RATE as f64) >= 0.25,
            "digit 1: no 1209 Hz"
        );

        let digit_period = (DIGIT_PERIOD_S * RATE as f64) as usize;
        let digit2 = f64s(&dialling[digit_period..digit_period + tone_len]);
        assert!(
            goertzel(&digit2, 770.0, RATE as f64) >= 0.25,
            "digit 5: no 770 Hz"
        );
        assert!(
            goertzel(&digit2, 1336.0, RATE as f64) >= 0.25,
            "digit 5: no 1336 Hz"
        );
        // Guards against a bug that always plays digit 1's pair
        // regardless of which digit was requested.
        assert!(
            goertzel(&digit2, 697.0, RATE as f64) <= 0.1,
            "digit 5 contains digit 1's row tone (697 Hz) - wrong digit played"
        );
    }

    /// Measures the ringback cadence from the rendered samples rather
    /// than asserting a fixed sample offset, using a windowed envelope
    /// (RMS over non-overlapping 10 ms windows) rather than raw sample
    /// values - a sine crosses zero twice a cycle, so "sample == 0.0"
    /// alone cannot tell a tone from silence. The gaps are exact zero
    /// samples (see `generate`'s `Ringback` arm), so the envelope
    /// threshold has no ambiguity to resolve.
    fn envelope_runs(samples: &[f32], win: usize) -> Vec<(bool, f64)> {
        let mut runs: Vec<(bool, usize)> = Vec::new();
        for chunk in samples.chunks(win) {
            let rms = (chunk.iter().map(|&x| (x as f64).powi(2)).sum::<f64>() / chunk.len() as f64)
                .sqrt();
            let active = rms > 0.05;
            match runs.last_mut() {
                Some((a, n)) if *a == active => *n += chunk.len(),
                _ => runs.push((active, chunk.len())),
            }
        }
        runs.into_iter()
            .map(|(a, n)| (a, n as f64 / RATE as f64))
            .collect()
    }

    /// Required test: UK ringback is 400 + 450 Hz with the 0.4/0.2/0.4/2.0
    /// s cadence, asserted on durations, not just frequencies. Mutation 3
    /// (halving the cadence) fails this directly - see the task report.
    ///
    /// The expected cadence below is written as literal seconds (0.4,
    /// 0.2, 0.4, 2.0), not as `RING_ON1_S` and friends. Comparing against
    /// this module's own constants would make the test invariant under
    /// exactly the mutation it exists to catch: halving all four
    /// constants together halves both what production renders and what
    /// the test expects, and the comparison still holds. Measured
    /// directly - this was this test's first draft, and it passed
    /// unchanged against a halved cadence (see the task report's
    /// Mutation 3). The published UK cadence is the independent fact
    /// this test has to check against, the same way `link.rs`'s CRC test
    /// checks against the standard check vector rather than its own
    /// encoder.
    #[test]
    fn ringback_is_400_450_with_the_uk_cadence() {
        let groups = render_all("1");
        let ring = stage_samples(&groups, Stage::Ringback);

        let on1 = f64s(&ring[..(0.4 * RATE as f64) as usize]);
        assert!(
            goertzel(&on1, 400.0, RATE as f64) >= 0.35,
            "no 400 Hz in first ring"
        );
        assert!(
            goertzel(&on1, 450.0, RATE as f64) >= 0.35,
            "no 450 Hz in first ring"
        );

        let runs = envelope_runs(ring, (0.01 * RATE as f64) as usize);
        let want = [(true, 0.4), (false, 0.2), (true, 0.4), (false, 2.0)];
        assert_eq!(
            runs.len(),
            want.len(),
            "ringback did not measure as four segments: {runs:?}"
        );
        for (i, ((got_active, got_s), (want_active, want_s))) in
            runs.iter().zip(want.iter()).enumerate()
        {
            assert_eq!(got_active, want_active, "segment {i}: wrong on/off state");
            assert!(
                (got_s - want_s).abs() <= 0.02,
                "segment {i}: measured {got_s:.3} s, expected {want_s:.3} s"
            );
        }
    }

    /// Off segments must be exact silence, not merely quiet - a stronger
    /// claim than the envelope threshold above, and one an amplitude-only
    /// mutant (e.g. a quieter tone instead of no tone) would not survive.
    #[test]
    fn ringback_off_segments_are_exact_silence() {
        let groups = render_all("1");
        let ring = stage_samples(&groups, Stage::Ringback);
        let off_start = (RING_ON1_S * RATE as f64) as usize;
        let off_end = off_start + (RING_OFF_S * RATE as f64) as usize;
        assert!(
            ring[off_start..off_end].iter().all(|&x| x == 0.0),
            "ringback's off segment is not exact silence"
        );
    }

    /// ANSam's carrier. A one-second window would span multiple phase
    /// reversals and cancel; this measures inside a single 100 ms window
    /// (well short of the 450 ms reversal interval), which at 8 kHz still
    /// gives an exact 210-cycle window for 2100 Hz.
    #[test]
    fn ansam_is_2100_hz() {
        let groups = render_all("1");
        let ansam = stage_samples(&groups, Stage::Ansam);
        let win = f64s(&ansam[..800]); // 100 ms
        assert!(
            goertzel(&win, 2100.0, RATE as f64) >= 0.4,
            "ANSam does not contain 2100 Hz"
        );
    }

    /// Required test, and the one the brief is most insistent on
    /// measuring rather than asserting against the constant: cross-
    /// correlates the rendered ANSam segment against a *fresh* reference
    /// 2100 Hz oscillator (same starting phase, never reversed) over
    /// short non-overlapping windows. The correlation's sign flips
    /// exactly where a real phase reversal sits, because this module
    /// implements a reversal as literally negating the carrier sample
    /// (`sin(x + pi) = -sin(x)`, so a sign flip *is* a 180 degree phase
    /// reversal) - see `generate`'s `Ansam` arm. The AM envelope and
    /// scale factor are both positive multipliers and so cannot affect
    /// that sign. Mutation 2 (remove the reversals) leaves the sign
    /// constant throughout and this test finds no transitions at all -
    /// see the task report.
    #[test]
    fn ansam_phase_reversal_interval_is_450ms_plus_or_minus_25ms() {
        let groups = render_all("1");
        let ansam = stage_samples(&groups, Stage::Ansam);

        let mut reference = Nco::new(ANSAM_FREQ, RATE as f64);
        let win = 80usize; // 10 ms, an exact 21-cycle window at 2100 Hz
        let mut signs = Vec::new();
        for chunk in ansam.chunks(win) {
            if chunk.len() < win {
                break;
            }
            let mut corr = 0.0f64;
            for &s in chunk {
                corr += s as f64 * reference.next();
            }
            signs.push(corr > 0.0);
        }

        let mut transitions = Vec::new();
        for i in 1..signs.len() {
            if signs[i] != signs[i - 1] {
                transitions.push(i);
            }
        }
        assert!(
            transitions.len() >= 2,
            "found {} phase reversal(s) in ANSam, expected several - \
             the carrier does not appear to be reversing phase at all",
            transitions.len()
        );

        let win_s = win as f64 / RATE as f64;
        for pair in transitions.windows(2) {
            let interval_s = (pair[1] - pair[0]) as f64 * win_s;
            assert!(
                (interval_s - ANSAM_REVERSAL_S).abs() <= 0.025,
                "measured reversal interval {interval_s:.3} s, want {ANSAM_REVERSAL_S} +/- 0.025 s"
            );
        }
    }

    /// Required test: the 15 Hz AM is present. Measured by taking the
    /// carrier's magnitude in each of many short (10 ms) windows across
    /// the whole ANSam stage - one magnitude sample every 10 ms, an
    /// effective 100 Hz "sample rate" for that envelope series - then
    /// running Goertzel on *that* series at 15 Hz. 10 ms windows align
    /// exactly with the 450 ms reversal boundaries (45 windows per half),
    /// so no window straddles a reversal and the magnitude series is a
    /// clean reading of the envelope alone.
    #[test]
    fn ansam_carries_15hz_amplitude_modulation() {
        let groups = render_all("1");
        let ansam = stage_samples(&groups, Stage::Ansam);
        let win = 80usize; // 10 ms
        let mut envelope_series = Vec::new();
        for chunk in ansam.chunks(win) {
            if chunk.len() < win {
                break;
            }
            let s = f64s(chunk);
            envelope_series.push(goertzel(&s, ANSAM_FREQ, RATE as f64));
        }
        let envelope_rate = 1.0 / (win as f64 / RATE as f64); // 100 Hz
        let mag = goertzel(&envelope_series, ANSAM_AM_FREQ, envelope_rate);
        assert!(
            mag >= 0.1,
            "no 15 Hz component found in ANSam's amplitude envelope: {mag}"
        );
    }

    /// Closes the gap Mutation 4 names directly: nothing in the required
    /// test list checks that training actually contains transitions, and
    /// a receiver's Gardner timing loop cannot acquire lock without them
    /// (see this module's doc). Measures dominance of the V.21 low
    /// channel's mark tone (980 Hz) against its space tone (1180 Hz) in
    /// consecutive 50 ms windows (an exact 15-symbol, integer-cycle
    /// window for both tones at 8 kHz) and asserts that dominance is not
    /// constant across the whole stage.
    #[test]
    fn training_stage_contains_transitions_not_a_steady_tone() {
        let groups = render_all("1");
        let training = stage_samples(&groups, Stage::Training);
        let win = 400usize; // 50 ms
        let mut mark_dominant = Vec::new();
        for chunk in training.chunks(win) {
            if chunk.len() < win {
                break;
            }
            let s = f64s(chunk);
            let m = goertzel(&s, V21_LOW_MARK, RATE as f64);
            let sp = goertzel(&s, V21_LOW_SPACE, RATE as f64);
            mark_dominant.push(m > sp);
        }
        assert!(
            mark_dominant.iter().any(|&d| d) && mark_dominant.iter().any(|&d| !d),
            "training never changes between mark and space tone dominance - \
             a Gardner timing loop cannot acquire lock on this"
        );
    }

    /// Own addition: CI must use V.21's *low* channel exclusively, not
    /// Bell 103 (either band) and not V.21's own high channel. Nothing in
    /// the required test list checks this, and a bug that emitted, say,
    /// Bell 103's answer band instead would still "contain a two-tone FSK
    /// signal" without anything here noticing - the aggregate-presence
    /// trap the brief names, applied to channel selection rather than a
    /// single frequency.
    ///
    /// Presence is measured as the *peak* Goertzel magnitude across many
    /// short (100 sample, 12.5 ms) windows, not a single whole-segment
    /// Goertzel: `FskChannel` keeps one continuous oscillator and simply
    /// retunes it at each symbol boundary (see its doc), so separate
    /// mark bursts are not phase-aligned with each other the way
    /// repeated cycles of one steady tone would be. A whole-segment
    /// Goertzel effectively sums those bursts with essentially random
    /// relative phase and reads close to zero (measured: 0.03-0.04 for
    /// this exact payload) even though the tone is genuinely present for
    /// a large fraction of the stage - measuring in short windows and
    /// taking the peak finds a window that lands on a real burst instead
    /// of averaging across many out-of-phase ones.
    #[test]
    fn ci_uses_only_the_v21_low_channel() {
        let groups = render_all("1");
        let ci = stage_samples(&groups, Stage::Ci);
        let win = 100usize; // 12.5 ms
        let mut peak_mark = 0.0f64;
        let mut peak_space = 0.0f64;
        for chunk in ci.chunks(win) {
            if chunk.len() < win {
                break;
            }
            let s = f64s(chunk);
            peak_mark = f64::max(peak_mark, goertzel(&s, V21_LOW_MARK, RATE as f64));
            peak_space = f64::max(peak_space, goertzel(&s, V21_LOW_SPACE, RATE as f64));
        }
        assert!(
            peak_mark >= 0.3,
            "CI never shows strong energy at the V.21 low channel's mark tone (980 Hz): peak {peak_mark}"
        );
        assert!(
            peak_space >= 0.3,
            "CI never shows strong energy at the V.21 low channel's space tone (1180 Hz): peak {peak_space}"
        );

        // Bell 103 originate's space tone, 1070 Hz, is deliberately not
        // checked here. Measured: it reads a windowed peak of 0.74 and a
        // whole-segment magnitude of 0.37 even in this genuinely
        // V.21-low-only signal - because 1070 Hz sits within about 10 Hz
        // of the true midpoint (1080 Hz) of the 980/1180 pair, and a
        // narrow-deviation, continuous-phase FSK signal at 300 baud (a
        // modulation index of 200 Hz / 300 Bd = 0.67) genuinely has no
        // spectral null there - that is a property of any correct
        // V.21-low renderer at this baud rate, not evidence of Bell 103
        // confusion, so it cannot be used as a discriminator.
        //
        // The other five are all far enough from 980/1180 (at least
        // ~470 Hz) that a genuinely V.21-low-only signal's peak there
        // stays clearly below the real tones' - measured 0.03-0.25
        // against 980/1180's own 0.54/0.79 - so a windowed-peak threshold
        // still works for them.
        for freq in [1270.0, 2225.0, 2025.0, 1650.0, 1850.0] {
            let mut peak = 0.0f64;
            for chunk in ci.chunks(win) {
                if chunk.len() < win {
                    break;
                }
                let s = f64s(chunk);
                peak = f64::max(peak, goertzel(&s, freq, RATE as f64));
            }
            assert!(
                peak <= 0.35,
                "CI has unexpected energy at {freq} Hz (peak {peak}) - not the V.21 low channel"
            );
        }
    }

    /// Own addition: pins each stage's duration to a documented sensible
    /// range, not only the overture's total. A mutant that shrinks one
    /// stage drastically while enlarging another to compensate would
    /// still pass a total-duration-only bound - exactly the aggregate
    /// trap the brief names, applied to duration rather than a frequency
    /// magnitude.
    #[test]
    fn each_stage_duration_is_individually_sensible() {
        let groups = render_all("1");
        let bounds: &[(Stage, f64, f64)] = &[
            (Stage::OffHook, 0.01, 0.2),
            (Stage::DialTone, 0.5, 3.0),
            (Stage::Dialling, 0.1, 1.0),
            (Stage::Ringback, 2.0, 6.0),
            (Stage::Ci, 0.05, 1.0),
            (Stage::Ansam, 2.0, 5.0),
            (Stage::CmJm, 0.05, 1.0),
            (Stage::Cj, 0.05, 1.0),
            (Stage::Training, 0.5, 3.0),
        ];
        for (stage, min_s, max_s) in bounds {
            let samples = stage_samples(&groups, *stage);
            let secs = samples.len() as f64 / RATE as f64;
            assert!(
                secs >= *min_s && secs <= *max_s,
                "{}: measured {secs:.3} s, expected between {min_s} and {max_s} s",
                stage.name()
            );
        }
    }

    /// Required test: the stage sequence reaches Connected, passing
    /// through every stage in order with none skipped or repeated out of
    /// sequence.
    #[test]
    fn stage_sequence_reaches_connected_in_order() {
        let groups = render_all("01234");
        let got: Vec<Stage> = groups.iter().map(|(s, _)| *s).collect();
        assert_eq!(
            got,
            vec![
                Stage::OffHook,
                Stage::DialTone,
                Stage::Dialling,
                Stage::Ringback,
                Stage::Ci,
                Stage::Ansam,
                Stage::CmJm,
                Stage::Cj,
                Stage::Training,
                Stage::Connected,
            ]
        );
    }

    /// Required test: the whole performance is well under a minute -
    /// "one that runs for a minute is wrong" per the brief - and also not
    /// degenerately short.
    #[test]
    fn total_duration_is_within_a_sensible_bound() {
        let groups = render_all("1234");
        let total: usize = groups.iter().map(|(_, v)| v.len()).sum();
        let secs = total as f64 / RATE as f64;
        assert!(secs > 3.0, "overture is suspiciously short: {secs:.3} s");
        assert!(
            secs < 20.0,
            "overture runs for {secs:.3} s - far too long a performance"
        );
    }

    /// `Stage::name` must not be empty for any variant - a cheap guard
    /// against a copy-paste that leaves one arm blank.
    #[test]
    fn every_stage_has_a_name() {
        for stage in [
            Stage::OffHook,
            Stage::DialTone,
            Stage::Dialling,
            Stage::Ringback,
            Stage::Ci,
            Stage::Ansam,
            Stage::CmJm,
            Stage::Cj,
            Stage::Training,
            Stage::Connected,
        ] {
            assert!(!stage.name().is_empty());
        }
    }

    /// Non-DTMF characters in the dialled digits are dropped rather than
    /// crashing or being played as tones they do not have. `ATDT01234`
    /// hands the digits straight through in this project's usage, but a
    /// dialling directory (Task 18) is free-text, so this cannot panic on
    /// stray characters.
    #[test]
    fn non_dtmf_characters_are_dropped_not_played() {
        let groups = render_all("1-2");
        let dialling = stage_samples(&groups, Stage::Dialling);
        let secs = dialling.len() as f64 / RATE as f64;
        // Two real digits, not three "characters".
        assert!(
            (secs - 2.0 * DIGIT_PERIOD_S).abs() <= 0.01,
            "dialling lasted {secs:.3} s, expected two digits' worth ({} s) - \
             the hyphen was played as a digit",
            2.0 * DIGIT_PERIOD_S
        );
    }
}
