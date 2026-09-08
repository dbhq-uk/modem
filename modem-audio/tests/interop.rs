//! Cross-validation against minimodem, the reference Bell 103
//! implementation.
//!
//! Every other test in this workspace proves modem-core agrees with
//! itself - the same `Tx` and the same `Rx`, sharing every convention by
//! construction. That is exactly the shape of bug this project has
//! already shipped once: Task 3's `Tx` popped its bit queue from the
//! wrong end, reversing every byte, and passed all fifteen of that task's
//! own tests, because `Rx` reversed them right back. Only an independent
//! implementation on one side can catch a defect both of *our* ends agree
//! on. minimodem is that independent implementation, so this file is what
//! makes "this is genuinely Bell 103" checkable rather than assumed.
//!
//! Every invocation below pins the role explicitly on both sides -
//! `--mark 1270 --space 1070`, `-8` (8-N-1), 300 baud - rather than
//! relying on any default either side happens to share. A baudmode of
//! `300` already implies Bell103 ASCII 8-N-1 in minimodem, but stating it
//! again costs nothing and this test exists specifically to not lean on
//! an implicit agreement.
//!
//! # The acquisition caveat, measured rather than assumed
//!
//! modem-core's `Rx` needs an alternating preamble to acquire symbol
//! timing from a cold start - see `modem_core::rx`'s own module doc.
//! minimodem's lead-in is not that: it is a short run of constant idle
//! mark, not alternating symbols. Measured directly (six payload lengths
//! from 5 to 160 characters, `minimodem --tx --file out.wav --mark 1270
//! --space 1070 300`, WAV duration read back with Python's `wave`
//! module, independently reconfirmed by review): the lead-in is a
//! constant 640 samples at this crate's default 48 kHz, 13.3333 ms, four
//! whole symbol periods, identical at every length tried. That is a
//! materially different number from an earlier probe of this same tool
//! that reported about 46 ms - stated here rather than quietly
//! reconciled, since this file's own number is the one reproduced
//! against the `minimodem 0.24` actually installed where these tests
//! run, and a future minimodem build is free to differ again.
//!
//! Because that lead-in is a *fixed* duration and modem-core's cold-start
//! pipeline delay (the resampler's group delay, then the correlator
//! window filling) is also fixed for a given device rate, the phase the
//! free-running Gardner clock arrives at when the payload's first real
//! transition appears is fixed too - not a fresh draw for every message.
//! Fix round 1 widened the sweep well past this file's original 48
//! trials: 9 device rates from 8000 to 96000 Hz, the answer band
//! (`--mark 2225 --space 2025`, `Role::Answer`) at 5 of those rates, and
//! 40 further random payloads on an independent seed - zero failures
//! throughout. A direct 800-point sweep (5 payloads x 160 sub-symbol
//! offsets at 48 kHz) pins the actual shape: **145 of 160 offsets (91%)
//! decode byte-exact from character 0, 2 of 160 lose exactly one leading
//! character, and 15 of 160 fail completely** - not a few corrupted
//! characters, but a decode that never finds byte alignment at all and
//! returns roughly 40% of the expected length (measured: 51 of 86 bytes
//! on a representative failing offset). The 48 kHz baseline this test
//! runs at sits 49 samples clear of the nearest losing offset going back
//! and 96 going forward - comfortably inside the 145-wide clean band,
//! not balanced on its edge.
//!
//! So the honest characterisation is: **this configuration decodes
//! byte-exact from the first character in the overwhelming majority of
//! cases, and when it does not, it fails hard and short rather than
//! trimming a few leading characters.** There is no graceful middle where
//! a receiver reads a handful of extra wrong characters and recovers -
//! `we_decode_minimodem_tx`'s length assertion fires first and fails the
//! test outright for that 15-of-160 mode, before the small
//! acquisition-loss window below is ever consulted. That window exists
//! for the real but rare 2-of-160 single-character-loss mode, not as a
//! hedge against a future minimodem build landing in a different band -
//! a build that shifted the lead-in into the losing band fails this
//! test's length check, loudly, rather than degrading into it.
//!
//! # This is a coarse conformance check, not the precise pin
//!
//! Both directions tolerate errors in the very constants they look like
//! they are pinning. Measured: our own baud rate can drift as far as 312
//! (+4%) before `minimodem_decodes_our_tx` notices, failing only at 315;
//! either tone can shift by 40 Hz before either direction notices,
//! failing only at 70 Hz on `minimodem_decodes_our_tx` and 100 Hz on
//! both directions. Neither minimodem's own receiver tolerance nor this
//! crate's is a fine-grained instrument, and this file should not be
//! read as one. The precise pins already exist elsewhere in this
//! workspace, unaffected by any of the above: `tx.rs`'s `tones_per_role`
//! asserts the exact tone values for both roles, and `tx.rs`'s
//! `samples_for_bits_is_fractional` asserts an exact rounded sample
//! count that only holds at `BAUD = 300.0` - both catch a change this
//! file's own tests do not (a 301 baud `Tx` fails `samples_for_bits_
//! is_fractional` immediately; `minimodem_decodes_our_tx` does not
//! notice until 315). What this file proves is that the signal is
//! genuinely decodable by an independent Bell 103 implementation, not a
//! replacement for those two, and a later task should not delete them on
//! the assumption this file already covers their ground.
//!
//! `we_decode_minimodem_tx` still searches a small window rather than
//! asserting bare equality, because the 2-of-160 single-character-loss
//! mode above is a real, if rare, outcome at this device rate. See
//! [`ACQUISITION_LOSS_CHARS`] for exactly what it does and does not
//! protect against.

use std::io::Write as _;
use std::process::{Command, Stdio};

use modem_audio::{read_wav, write_wav};
use modem_core::rx::Rx;
use modem_core::tx::Tx;
use modem_core::{Config, Duplex, Role};

/// Originate band, pinned explicitly on every minimodem invocation below.
const MARK_HZ: &str = "1270";
const SPACE_HZ: &str = "1070";
const BAUD: &str = "300";
const SAMPLE_RATE: u32 = 48000;

/// How many leading characters `we_decode_minimodem_tx` allows to be
/// wrong before it requires the remainder byte-exact.
///
/// This is **not** a hedge against a different minimodem build or device
/// rate landing in a different sub-symbol band - see this file's module
/// doc for the measured failure shape. A build that landed in one of the
/// 15-of-160 losing offsets fails the length assertion above this search
/// outright, and no size of this window would soften that; it fails
/// short and loud, not gracefully. What this constant actually covers is
/// the real, if rare, single-character loss mode measured directly: 2 of
/// 160 sub-symbol offsets in an 800-point sweep (5 payloads x 160
/// offsets at 48 kHz) lose exactly the first character and recover
/// byte-exact from the second. Two is generous headroom over that
/// measured one, without weakening what this test actually proves - the
/// search below still requires the located alignment to be exact from
/// that point to the end of an 86-byte payload, not merely close.
const ACQUISITION_LOSS_CHARS: usize = 2;

fn minimodem_available() -> bool {
    Command::new("minimodem")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn temp_wav_path(tag: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "modem_audio_interop_{tag}_{}.wav",
        std::process::id()
    ))
}

/// Renders `payload` with our own `Tx` at `SAMPLE_RATE`: idle-mark settle,
/// then the payload, then trailing margin. No alternating preamble - that
/// is this crate's own acquisition precondition, and minimodem's receiver
/// is not the thing that needs it; this direction has no acquisition
/// caveat at all, per this file's module doc.
fn render_with_our_tx(payload: &[u8]) -> Vec<f32> {
    let cfg = Config {
        sample_rate: SAMPLE_RATE,
        role: Role::Originate,
        duplex: Duplex::HalfPingPong,
    };
    let mut tx = Tx::new(cfg);
    let mut samples = Vec::new();
    let mut buf = vec![0.0f32; 4096];

    let settle = SAMPLE_RATE as usize / 5; // 200 ms idle mark
    let mut emitted = 0;
    while emitted < settle {
        tx.read(&mut buf);
        samples.extend_from_slice(&buf);
        emitted += buf.len();
    }

    tx.write(payload);
    // Device-rate sample count for the payload's bits, plus a full
    // second of margin - the same convention rx.rs's own loopback tests
    // use for a device rate that need not equal DSP_RATE.
    let total = (payload.len() * 10) * SAMPLE_RATE as usize / 300 + SAMPLE_RATE as usize;
    let mut emitted = 0;
    while emitted < total {
        tx.read(&mut buf);
        samples.extend_from_slice(&buf);
        emitted += buf.len();
    }
    samples
}

#[test]
fn minimodem_decodes_our_tx() {
    if !minimodem_available() {
        eprintln!("SKIP (minimodem_decodes_our_tx): minimodem not found on PATH");
        return;
    }
    println!("RAN (minimodem_decodes_our_tx): rendering with our Tx, decoding with minimodem --rx");

    let payload = b"CONNECT 300 The quick brown fox jumps over the lazy dog. 0123456789";
    let samples = render_with_our_tx(payload);

    let wav_path = temp_wav_path("tx");
    let file = std::fs::File::create(&wav_path).expect("create wav file for our Tx output");
    write_wav(file, &samples, SAMPLE_RATE);

    let output = Command::new("minimodem")
        .args([
            "--rx",
            "--file",
            wav_path.to_str().unwrap(),
            "--mark",
            MARK_HZ,
            "--space",
            SPACE_HZ,
            "-8",
            "--quiet",
            BAUD,
        ])
        .output()
        .expect("run minimodem --rx");
    std::fs::remove_file(&wav_path).ok();

    assert!(
        output.status.success(),
        "minimodem --rx exited with failure, status {:?}, stderr {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        output.stdout,
        payload,
        "minimodem did not decode our Tx output byte-exact; got {:?}",
        String::from_utf8_lossy(&output.stdout)
    );
}

#[test]
fn we_decode_minimodem_tx() {
    if !minimodem_available() {
        eprintln!("SKIP (we_decode_minimodem_tx): minimodem not found on PATH");
        return;
    }
    println!("RAN (we_decode_minimodem_tx): generating with minimodem --tx, decoding with our Rx");

    let payload =
        b"minimodem to modem-core: The quick brown fox jumps over the lazy dog. 0123456789 !@#$%";
    let wav_path = temp_wav_path("rx");

    let mut child = Command::new("minimodem")
        .args([
            "--tx",
            "--file",
            wav_path.to_str().unwrap(),
            "--mark",
            MARK_HZ,
            "--space",
            SPACE_HZ,
            "-8",
            "-R",
            &SAMPLE_RATE.to_string(),
            "--quiet",
            BAUD,
        ])
        .stdin(Stdio::piped())
        .spawn()
        .expect("spawn minimodem --tx");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(payload)
        .expect("write payload to minimodem stdin");
    let status = child.wait().expect("wait for minimodem --tx");
    assert!(status.success(), "minimodem --tx exited with failure");

    let file = std::fs::File::open(&wav_path).expect("open minimodem's generated wav");
    let (samples, rate) = read_wav(file);
    std::fs::remove_file(&wav_path).ok();
    assert_eq!(
        rate, SAMPLE_RATE,
        "minimodem wrote a different sample rate than expected - the probed fact this file's \
         doc relies on has changed"
    );

    // Role-fix call-site audit: minimodem was told to transmit at
    // `--mark 1270 --space 1070` (MARK_HZ/SPACE_HZ, Originate's own band),
    // independent of this crate's `Role` enum. Post Role-fix, `Rx::new`
    // listens on `tones(cfg.role.listen())`, so to listen on 1270/1070
    // this `Rx` must be configured `Role::Answer` -
    // tones(Answer.listen()) == tones(Originate) == 1270/1070. Before the
    // fix, `Role::Originate` here happened to work only because `Rx::new`
    // wrongly used `tones(cfg.role)` directly.
    let cfg = Config {
        sample_rate: rate,
        role: Role::Answer,
        duplex: Duplex::HalfPingPong,
    };
    let mut rx = Rx::new(cfg);
    let mut out = Vec::new();
    let mut got = [0u8; 4096];
    for chunk in samples.chunks(733) {
        rx.write(chunk);
        let n = rx.read(&mut got);
        out.extend_from_slice(&got[..n]);
    }

    // No inserted or dropped bytes - only the front end is allowed to be
    // wrong, never the length. A cycle slip that drops or duplicates a
    // bit produces exactly this signature (Task 5's own finding, see
    // modem-core::impair's module doc), and it must not be scored as
    // "found some alignment" by the search below.
    assert_eq!(
        out.len(),
        payload.len(),
        "decoded length {} does not match payload length {} - bytes were inserted or dropped, \
         not just corrupted at the front; got {:?}",
        out.len(),
        payload.len(),
        String::from_utf8_lossy(&out)
    );

    // Search only the small, documented acquisition window - not Task
    // 8's ALIGN_SEARCH, which is deliberately wide and fails safe for a
    // release gate rather than proving anything about a specific
    // receiver property. Finding alignment 0 here (which is what this
    // crate's own measurement shows today) still walks this loop and
    // proves it directly, rather than assuming it.
    let loss = (0..=ACQUISITION_LOSS_CHARS)
        .find(|&n| out[n..] == payload[n..])
        .unwrap_or_else(|| {
            panic!(
                "no alignment within the documented {ACQUISITION_LOSS_CHARS}-character \
                 acquisition window recovered the payload byte-exact; got {:?}",
                String::from_utf8_lossy(&out)
            )
        });

    println!("we_decode_minimodem_tx: measured acquisition loss = {loss} character(s)");

    assert_eq!(
        &out[loss..],
        &payload[loss..],
        "payload after the first {loss} acquisition character(s) was not byte-exact"
    );
}
