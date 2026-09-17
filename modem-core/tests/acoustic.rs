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
use modem_core::session::IDLE_MARK_AMPLITUDE;
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
///
/// Emitted at `IDLE_MARK_AMPLITUDE`, the level `Session` actually holds
/// it at, so the gains swept below are *acoustic* ratios - how much
/// louder this device's own speaker is at its own microphone than the
/// far device is - rather than raw sample amplitudes. That is the number
/// a room decides, and it is set by geometry: at 2 cm from your own
/// speaker and 15 cm from theirs, inverse square puts it around 7x
/// before anything electrical is involved.
fn own_speaker(role: Role, len: usize) -> Vec<f32> {
    let mut tx = Tx::new(Config {
        sample_rate: RATE,
        role,
        duplex: Duplex::Full,
    });
    let mut air = vec![0.0f32; len];
    tx.read(&mut air);
    for s in air.iter_mut() {
        *s *= IDLE_MARK_AMPLITUDE;
    }
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

/// Both bands must clear the self-jam measured on real hardware.
///
/// This used to assert that the answer band tolerated *less* than the
/// originate band, which was the finding at the time and is no longer
/// true: at `IDLE_MARK_AMPLITUDE` they both clear the whole sweep. The
/// asymmetry has not gone away - it is a property of the band plan - but
/// the idle tone is now quiet enough that neither band reaches it.
///
/// So the threshold is the field measurement instead, which is a better
/// anchor than a comparison between the two. On 15 September 2026 a
/// Windows laptop and an iPhone 10-20 cm apart measured the laptop's own
/// idle mark at 0.117 against 0.00525 for the phone's data tone - a
/// **22x** advantage for its own loudspeaker. See
/// docs/acoustic-harness.md.
///
/// # 22x IS THE ACOUSTIC PATH GAIN, NOT WHAT THE MICROPHONE HEARS NOW
///
/// This distinction is worth stating because the review of 16 September
/// 2026 read the constant the other way, and either reading is plausible
/// from the number alone (issue #9).
///
/// 0.117 was measured with the idle mark at full amplitude, before
/// `IDLE_MARK_AMPLITUDE` existed. So 22x is what geometry contributes:
/// how much louder this device's own loudspeaker is at its own
/// microphone than the far device is, for the same amplitude at source.
///
/// `own_speaker` below emits at `IDLE_MARK_AMPLITUDE`, so the sweep's
/// gain is that path gain and the ratio actually presented to the
/// demodulator is `gain * IDLE_MARK_AMPLITUDE`. At 22 that is **0.44x** -
/// which is the correct number, because the same room today attenuates
/// its own idle mark by the same 0.02: (0.117 x 0.02) / 0.00525 = 0.446.
///
/// The test below is therefore modelling the room as it is now, and the
/// constant is the right value. What it does *not* show is that a raw
/// 22x at the microphone is survivable - it is not, and
/// `the_idle_attenuation_is_what_makes_this_work` pins that.
const FIELD_SELF_JAM: f32 = 22.0;

#[test]
fn both_bands_clear_the_self_jam_measured_in_the_field() {
    for role in [Role::Originate, Role::Answer] {
        let base = transmit(role, PAYLOAD);
        let own = own_speaker(far(role), base.len());
        let mut worst_ok = 0.0f32;
        for gain in [0.0f32, 4.0, 8.0, 12.0, 16.0, 22.0, 30.0, 40.0] {
            let air = duplex_leak(&base, &own, gain);
            if ber_of(role, &air) <= WORKING {
                worst_ok = gain;
            } else {
                break;
            }
        }
        println!("LEAK {role:?} tolerated up to {worst_ok}x its own loudspeaker");
        assert!(
            worst_ok >= FIELD_SELF_JAM,
            "{role:?} tolerates only {worst_ok}x its own loudspeaker, under the {FIELD_SELF_JAM}x \
             measured on real hardware. Two devices on a desk would fail in this direction - \
             which is exactly what was happening before IDLE_MARK_AMPLITUDE was measured rather \
             than modelled"
        );
    }
}

/// The idle attenuation is load-bearing, not cosmetic.
///
/// Added 17 September 2026 (issue #9). The review that prompted it ran
/// the sweep above against an idle mark at *full* amplitude - the raw
/// 22x the field measured before `IDLE_MARK_AMPLITUDE` existed - and got
/// 98% byte errors. That is a fact worth owning a test, because nothing
/// else here would notice if the attenuation were removed: the sweep
/// above scales by `IDLE_MARK_AMPLITUDE` itself, so deleting it would
/// change what that test models without changing whether it passes.
///
/// `ToneDominance` discounting the receiver's own band is what made
/// two-device calls work at all, and it is easy to remember that as the
/// whole fix. It is half of it. At the level the idle mark ran at
/// before, the link does not decode in either band, however well carrier
/// detect behaves.
#[test]
fn the_idle_attenuation_is_what_makes_this_work() {
    for role in [Role::Originate, Role::Answer] {
        let base = transmit(role, PAYLOAD);

        // The same own-speaker tone, at the amplitude it ran at before
        // IDLE_MARK_AMPLITUDE - so `own_speaker`'s 0.02 is divided back
        // out rather than a second generator being written here that
        // could drift from it.
        let quiet = own_speaker(far(role), base.len());
        let loud: Vec<f32> = quiet.iter().map(|s| s / IDLE_MARK_AMPLITUDE).collect();

        let air = duplex_leak(&base, &loud, FIELD_SELF_JAM);
        let ber = ber_of(role, &air);
        println!("LOUD IDLE {role:?} ber={ber:.4}");
        assert!(
            ber > WORKING,
            "{role:?} decoded cleanly at {FIELD_SELF_JAM}x with the idle mark at full \
             amplitude (ber {ber:.4}). Either the band plan has changed or this test has \
             stopped modelling what it claims - the whole point of IDLE_MARK_AMPLITUDE is \
             that this case does NOT work"
        );
    }
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

/// Fixed-level noise, independent of the signal - which is what a room
/// actually is.
///
/// `impair::add_awgn` takes an SNR, so its noise scales down with the
/// tone and can never show an absolute floor being approached. That
/// matters here specifically: the question is how *quiet* idle mark can
/// get before a receiver stops hearing it, and a relative-SNR model
/// answers "arbitrarily quiet" no matter what the truth is.
fn fixed_noise(air: &mut [f32], amplitude: f32, seed: u64) {
    let mut x = seed | 1;
    for s in air.iter_mut() {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        let u = ((x >> 11) as f64 / (1u64 << 53) as f64) * 2.0 - 1.0;
        *s += amplitude * u as f32;
    }
}

/// What bounds `IDLE_MARK_AMPLITUDE` from below, measured.
///
/// Idle mark exists to keep the far end's carrier up between bursts, so
/// how quiet it can be is decided by carrier detection and nothing else -
/// data is transmitted from `tx` at full scale regardless.
///
/// This turned out not to bind at the levels that matter: detection
/// holds down to 0.1 against room noise at the *same level as the tone*,
/// because `ToneDominance` is a ratio test and scaling the tone scales
/// both sides of it. The chosen 0.2 therefore keeps a factor of two over
/// the lowest level measured working, and the acoustic tolerance above
/// is what actually picked it.
#[test]
fn carrier_survives_a_quiet_idle_tone() {
    for noise in [0.01f32, 0.03, 0.06, 0.1] {
        let mut tx = Tx::new(Config {
            sample_rate: RATE,
            role: Role::Originate,
            duplex: Duplex::Full,
        });
        let mut air = vec![0.0f32; RATE as usize];
        tx.read(&mut air);
        for s in air.iter_mut() {
            *s *= IDLE_MARK_AMPLITUDE;
        }
        fixed_noise(&mut air, noise, 0xC0FF_EE01);

        let mut rx = Rx::new(Config {
            sample_rate: RATE,
            role: Role::Answer,
            duplex: Duplex::Full,
        });
        let mut buf = vec![0u8; 256];
        for chunk in air.chunks(256) {
            rx.write(chunk);
            rx.read(&mut buf);
        }
        assert!(
            rx.carrier_detected(),
            "idle mark at IDLE_MARK_AMPLITUDE ({IDLE_MARK_AMPLITUDE}) was not detected as \
             carrier against fixed room noise at {noise}. That is the floor this constant \
             has to clear - if it no longer does, raise the constant rather than lowering \
             this test"
        );
    }
}
