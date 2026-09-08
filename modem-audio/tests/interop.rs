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
//! module): the lead-in is a constant 640 samples at this crate's default
//! 48 kHz, 13.333 ms, four whole symbol periods, identical at every
//! length tried. That is a materially different number from an earlier
//! probe of this same tool that reported about 46 ms - stated here rather
//! than quietly reconciled, since this file's own number is the one
//! reproduced against the `minimodem 0.24` actually installed where these
//! tests run, and a future minimodem build is free to differ again.
//!
//! Because that lead-in is a *fixed* duration and modem-core's cold-start
//! pipeline delay (the resampler's group delay, then the correlator
//! window filling) is also fixed for a given device rate, the phase the
//! free-running Gardner clock arrives at when the payload's first real
//! transition appears is fixed too - not a fresh draw for every message.
//! Measured across 48 trials at 48 kHz (8 hand-picked payloads exercising
//! different leading bit patterns, plus 40 further payloads of random
//! length 20-199 bytes and random printable content from a fixed-seed
//! xorshift generator - see the task report for the full sweep) that
//! phase lands in one of the sub-symbol offsets `rx.rs`'s own module doc
//! already documents as clean without an alternating preamble: every
//! single trial recovered its payload byte-exact from the very first
//! character, zero acquisition loss.
//!
//! `we_decode_minimodem_tx` still searches a small window rather than
//! asserting bare equality, because the zero measured here is a property
//! of this exact minimodem build's lead-in duration and this crate's
//! exact pipeline delay at 48 kHz, not a guarantee either of them owes
//! the other. [`ACQUISITION_LOSS_CHARS`] documents the margin and why.

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
/// Measured acquisition loss against the real `minimodem 0.24` on this
/// machine, at 48 kHz, across 48 varied trials (see this file's module
/// doc and the task report): zero, every time. This constant is not that
/// measured value - it is a small margin above it, because the zero is a
/// property of one specific, fixed lead-in duration meeting one specific,
/// fixed pipeline delay, and a different minimodem build, or a different
/// device rate, is free to land the same arithmetic in a different
/// sub-symbol band. `rx.rs`'s own module doc measures that band as one
/// symbol period wide out of 54 possible offsets; two characters is
/// generous headroom against a shift into a neighbouring one without
/// weakening what this test actually proves - the search below still
/// requires the located alignment to be exact from that point to the end
/// of a 70+ byte payload, not merely close.
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

    let cfg = Config {
        sample_rate: rate,
        role: Role::Originate,
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
