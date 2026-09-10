//! What a real acoustic call does to a Bell 103 link, measured.
//!
//! Two devices on a desk are not a wire, and the site's two-device mode
//! kept failing in the field while every test in the crate passed. The
//! crate's existing impairment tests each apply one impairment and check
//! it did what it says, which measures the impairment rather than the
//! link. This composes the chain a desk-to-desk call actually goes
//! through and reports a byte error rate for it.
//!
//! # What this found
//!
//! The investigation started from a plausible theory - browser noise
//! suppression removes sustained tones, and this modem is made of them -
//! and that theory is **wrong** at desk levels. Measured here, noise
//! suppression and automatic gain control both leave the link at zero
//! errors. The whole loss comes from one term, and it is self-inflicted:
//!
//! **Each end hears its own loudspeaker far louder than it hears the
//! other device**, and this project's half-duplex discipline has the end
//! without the turn transmit *continuous idle mark* (see `session.rs`).
//! So while one end talks, the other holds a full-amplitude tone inches
//! from its own microphone, and that tone is the dominant thing its
//! receiver has to see past.
//!
//! # And it is asymmetric, which is why only one direction failed
//!
//! The interferer is each end's own **mark** tone, the top of its own
//! band:
//!
//! | Receiving end | Listening on   | Own mark | Gap    |
//! |---------------|----------------|----------|--------|
//! | Originate     | 2025 / 2225 Hz | 1270 Hz  | 755 Hz |
//! | Answer        | 1070 / 1270 Hz | 2225 Hz  | 955 Hz |
//!
//! The originating end's own tone sits 200 Hz closer to the band it has
//! to decode than the answering end's does, and that is enough. Measured
//! below: the originate band is clean at four times its own speaker, the
//! answer band is not clean at three.
//!
//! That is the exact asymmetry reported from the field - the
//! answer-to-originate direction failing while originate-to-answer
//! worked - and it is a property of Bell 103's band plan used
//! acoustically, not of anything this crate chose.
//!
//! Nothing here changes the protocol. These tests exist so the numbers
//! are on the record and any later change to them is visible.

use modem_core::impair::{
    add_awgn, agc, band_limit, duplex_leak, measure_ber, noise_suppression, reverb,
};
use modem_core::rx::Rx;
use modem_core::tx::Tx;
use modem_core::{Config, Duplex, Role};

const RATE: u32 = 48_000;
const PAYLOAD: &[u8] = b"The quick brown fox jumps over the lazy dog. 0123456789. \
                         The quick brown fox jumps over the lazy dog. 0123456789.";

/// Anything at or below this counts as a working link here. Same order as
/// the crate's own release gate (`impair::MAX_BYTE_ERROR_RATE`), stated
/// separately so changing one is a deliberate change to that one.
const WORKING: f64 = 0.01;

/// A desk reflection: about 3 ms, which at 300 baud lands inside a
/// symbol.
const DESK_ECHO_SECS: f64 = 0.003;
const DESK_ECHO_GAIN: f32 = 0.4;
/// Room noise.
const ROOM_SNR_DB: f64 = 20.0;

fn far(role: Role) -> Role {
    match role {
        Role::Originate => Role::Answer,
        Role::Answer => Role::Originate,
    }
}

/// Modulates `payload` at `role`'s own tone pair and returns the air.
///
/// Preamble, then the idle-mark gap, then data - the exact burst shape
/// `Session::grant_turn` emits, so what is measured here is what a real
/// call puts on the wire.
fn transmit(role: Role, payload: &[u8]) -> Vec<f32> {
    let mut tx = Tx::new(Config {
        sample_rate: RATE,
        role,
        duplex: Duplex::Full,
    });
    tx.write(&[0x55, 0x55]);
    tx.write_idle_mark(20);
    tx.write(payload);
    let mut air = vec![0.0f32; (RATE as usize / 300) * 10 * (payload.len() + 40)];
    tx.read(&mut air);
    air
}

/// Demodulates air that `sent_by` transmitted.
///
/// `Rx::new` tunes to `tones(cfg.role.listen())` - the *far* end's band,
/// which is what a receiver actually wants - so the Rx that hears a
/// `sent_by` transmitter is the one configured as its counterpart.
/// Pairing the same role on both sides tunes the receiver to entirely the
/// wrong tones and decodes noise. This harness did exactly that on its
/// first run and reported a byte error rate of 1.0 for every condition
/// including the unimpaired one, which would have read as "the acoustic
/// path destroys the link" had the control not been checked first.
/// `an_unimpaired_link_is_perfect` is that control, kept.
///
/// Fed and drained block by block, the way an audio callback does.
fn receive(sent_by: Role, air: &[f32]) -> Vec<u8> {
    let mut rx = Rx::new(Config {
        sample_rate: RATE,
        role: far(sent_by),
        duplex: Duplex::Full,
    });
    let mut out = Vec::new();
    let mut buf = vec![0u8; 1024];
    for chunk in air.chunks(256) {
        rx.write(chunk);
        let n = rx.read(&mut buf);
        out.extend_from_slice(&buf[..n]);
    }
    out
}

/// Continuous idle mark from `role` - what `session.rs` has an end
/// transmit whenever it does not hold the turn, and therefore what that
/// device's own microphone hears from its own loudspeaker.
fn own_speaker(role: Role, len: usize) -> Vec<f32> {
    let mut tx = Tx::new(Config {
        sample_rate: RATE,
        role,
        duplex: Duplex::Full,
    });
    let mut air = vec![0.0f32; len];
    tx.read(&mut air);
    air
}

fn ber_of(role: Role, air: &[f32]) -> f64 {
    measure_ber(PAYLOAD, &receive(role, air))
}

/// The control. Without it every other number in this file is
/// unreadable - see `receive`'s own note on the run where it was missing.
#[test]
fn an_unimpaired_link_is_perfect() {
    for role in [Role::Originate, Role::Answer] {
        let ber = ber_of(role, &transmit(role, PAYLOAD));
        assert!(
            ber <= WORKING,
            "{role:?}'s band failed with no impairment at all ({ber:.4}) - \
             the harness is wrong, not the modem"
        );
    }
}

/// The headline finding, as a threshold rather than a verdict.
///
/// Both bands get the same desk: the same reflection, the same room
/// noise, the same amount of their own loudspeaker. One survives four
/// times its own speaker and the other does not survive three.
#[test]
fn answer_band_tolerates_less_of_its_own_speaker_than_originate_does() {
    let mut limits = Vec::new();
    for role in [Role::Originate, Role::Answer] {
        let base = transmit(role, PAYLOAD);
        let own = own_speaker(far(role), base.len());
        let mut worst_ok = 0.0f32;
        for gain in [0.0f32, 0.25, 0.5, 1.0, 1.5, 2.0, 3.0, 4.0] {
            let air = duplex_leak(&base, &own, gain);
            if ber_of(role, &air) <= WORKING {
                worst_ok = gain;
            } else {
                break;
            }
        }
        println!("LEAK {role:?} tolerated up to {worst_ok}x its own loudspeaker");
        limits.push(worst_ok);
    }
    let (originate, answer) = (limits[0], limits[1]);
    assert!(
        originate >= 4.0,
        "the originate band no longer tolerates 4x its own speaker ({originate}x) - \
         this is the direction that worked in the field"
    );
    assert!(
        answer < originate,
        "the two bands now tolerate their own loudspeaker equally (originate {originate}x, \
         answer {answer}x). If that is real it is an improvement, and this test and this \
         module's explanation of the field asymmetry both need rewriting"
    );
    assert!(
        answer >= 1.0,
        "the answer band cannot tolerate even equal-level leak ({answer}x) - \
         two-device mode would be unusable rather than merely fragile"
    );
}

/// The theory this investigation started from, disproved.
///
/// Noise suppression subtracts a learned stationary noise floor, and a
/// Bell 103 mark tone is stationary, so it looked like the obvious
/// culprit. At these levels it is not: against the same desk, both it
/// and automatic gain control leave the link working.
///
/// `web/modem.js` should still ask for them off and still report when a
/// device refuses - failing to measure harm here is not proof there is
/// none on real hardware, where a suppressor is far more aggressive than
/// this one-band model. But the *ordering* of the advice follows the
/// measurement: the thing to fix first is how loud each device's own
/// speaker is in its own microphone, not the browser's audio processing.
#[test]
fn browser_audio_processing_is_not_what_breaks_this() {
    // The fragile direction, so this is the fair test.
    let role = Role::Answer;
    let base = transmit(role, PAYLOAD);
    let own = own_speaker(far(role), base.len());

    // A desk the link genuinely survives, so any failure below is the
    // processing rather than the room.
    let mut desk = base.clone();
    band_limit(&mut desk, RATE as f64);
    reverb(
        &mut desk,
        (RATE as f64 * DESK_ECHO_SECS) as usize,
        DESK_ECHO_GAIN,
    );
    let mut desk = duplex_leak(&desk, &own, 1.0);
    add_awgn(&mut desk, ROOM_SNR_DB, 0x5EED_1234);
    let baseline = ber_of(role, &desk);
    assert!(
        baseline <= WORKING,
        "the desk baseline itself failed: {baseline:.4}"
    );

    let mut suppressed = desk.clone();
    noise_suppression(&mut suppressed, RATE as f64, 0.25, 1.0);
    let with_ns = ber_of(role, &suppressed);

    let mut gained = desk.clone();
    agc(&mut gained, RATE as f64, 0.2, 0.02);
    let with_agc = ber_of(role, &gained);

    println!("PROCESSING baseline={baseline:.4} ns={with_ns:.4} agc={with_agc:.4}");
    assert!(
        with_ns <= WORKING && with_agc <= WORKING,
        "browser audio processing now breaks this link (noise suppression {with_ns:.4}, \
         AGC {with_agc:.4}) where it did not before. That would make it the first thing \
         to tell people to turn off, ahead of speaker level - update the site's advice."
    );
}

/// The room on its own, without the self-jamming term, so the finding
/// above cannot be an artefact of an unreasonably harsh channel.
#[test]
fn the_room_alone_is_survivable_in_both_bands() {
    for role in [Role::Originate, Role::Answer] {
        let mut air = transmit(role, PAYLOAD);
        band_limit(&mut air, RATE as f64);
        reverb(
            &mut air,
            (RATE as f64 * DESK_ECHO_SECS) as usize,
            DESK_ECHO_GAIN,
        );
        add_awgn(&mut air, ROOM_SNR_DB, 0x0DD1_4E55);
        let ber = ber_of(role, &air);
        println!("ROOM {role:?} ber={ber:.4}");
        assert!(
            ber <= WORKING,
            "{role:?}'s band no longer survives band-limiting, a desk reflection and \
             {ROOM_SNR_DB} dB room noise ({ber:.4}) - the acoustic advice on the site \
             assumes it does"
        );
    }
}
