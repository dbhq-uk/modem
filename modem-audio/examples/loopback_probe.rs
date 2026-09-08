//! Hand-verification harness for Task 13's follow-up (the `run()` pacing
//! fix), run under `sg audio -c` against a real `snd-aloop` loopback
//! device. Committed - this is the only thing in the workspace that can
//! exercise `CpalTransport` against genuine ALSA duplex timing, and it
//! found the defect this fix closes (see `cpal_device.rs`'s own doc and
//! the task report).
//!
//! `run()` never blocks (see `transport.rs`'s own doc), but it is *not*
//! guaranteed to advance by exactly one block any more: it drains every
//! whole block of real captured input actually queued and generates
//! output for every whole block of room actually available, so a caller
//! that calls it slightly slow or slightly fast is no longer silently
//! corrupting the link - see `RunStats`. This harness still paces each
//! loop with roughly one block period of sleep between calls, matching
//! how a real caller (a future TUI's event loop) would behave; the
//! difference this fix makes is that being slightly off that pacing no
//! longer breaks the exchange.
//!
//! Every multi-`run()` loop below accumulates `RunStats` and prints the
//! totals - blocks actually processed each way and, crucially,
//! `input_samples_dropped` - so a genuinely lossy run is visible in the
//! output rather than merely inferred from "did the payload arrive".
//!
//! Run with:
//!   sg audio -c 'cargo run --release --example loopback_probe -p modem-audio'

use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait};
use modem_audio::{CpalTransport, Transport};
use modem_core::session::{Session, SessionState};
use modem_core::{Config, Duplex, Role};

const BLOCK: usize = 256;

fn pace(sample_rate: u32) -> Duration {
    Duration::from_secs_f64(BLOCK as f64 / sample_rate as f64)
}

/// Runs `transport` once, folding the returned `RunStats` into the
/// running totals so a caller can report, at the end of a loop, how much
/// work actually happened and how much input was genuinely lost - never
/// inferring either from timing alone.
fn run_and_accumulate(
    transport: &mut CpalTransport,
    ends: &mut [Session],
    input_blocks: &mut usize,
    output_blocks: &mut usize,
    dropped: &mut u64,
) -> Result<(), modem_audio::TransportError> {
    let stats = transport.run(ends)?;
    *input_blocks += stats.input_blocks;
    *output_blocks += stats.output_blocks;
    *dropped += stats.input_samples_dropped;
    Ok(())
}

fn main() {
    let host = cpal::default_host();
    println!("=== enumeration ===");
    if let Ok(devices) = host.output_devices() {
        for d in devices {
            println!("output device: {d}");
            if let Ok(cfg) = d.default_output_config() {
                println!("  default_output_config: {cfg:?}");
            }
        }
    }
    if let Ok(devices) = host.input_devices() {
        for d in devices {
            println!("input device: {d}");
            if let Ok(cfg) = d.default_input_config() {
                println!("  default_input_config: {cfg:?}");
            }
        }
    }

    println!("\n=== step 1+2: CpalTransport::new(false), one Session dialling ===");
    let mut transport = match CpalTransport::new(false) {
        Ok(t) => t,
        Err(e) => {
            println!("CpalTransport::new failed: {e}");
            return;
        }
    };
    let real_rate = transport.sample_rate();
    println!("negotiated sample_rate = {real_rate}");

    let cfg = transport.config_for(Role::Originate, Duplex::HalfPingPong);
    println!("Session Config built from transport: {cfg:?}");
    let mut session = Session::new(cfg);
    let start = Instant::now();
    session.dial("5551234567");
    let mut ends = [session];
    let period = pace(real_rate);
    let mut connected_wall_time = None;
    let deadline = Duration::from_secs(20);
    let (mut in_blocks, mut out_blocks, mut dropped) = (0usize, 0usize, 0u64);
    while start.elapsed() < deadline {
        if let Err(e) = run_and_accumulate(
            &mut transport,
            &mut ends,
            &mut in_blocks,
            &mut out_blocks,
            &mut dropped,
        ) {
            println!("run() failed: {e}");
            return;
        }
        if connected_wall_time.is_none() && ends[0].state() == SessionState::Connected {
            connected_wall_time = Some(start.elapsed());
            break;
        }
        std::thread::sleep(period);
    }
    println!(
        "single-session dial: state={:?} connected_after={:?} (overture.rs's own figure: ~11.0s at 48kHz - this is real ALSA wall-clock pacing, not a tight synthetic loop)",
        ends[0].state(),
        connected_wall_time
    );
    println!(
        "run() totals: input_blocks={in_blocks} output_blocks={out_blocks} input_samples_dropped={dropped}"
    );
    println!(
        "note: this session's own Rx listens on the opposite band to its own Tx by design (Role's whole point) - it will not decode its own transmission even over a perfect loopback. The two-session cases below are the meaningful decode tests."
    );
    drop(transport);
    drop(ends);

    println!("\n=== step 3: rate-mismatch demonstration (asserts on a decoded payload, never on reaching Connected) ===");
    println!(
        "Connected proves nothing about the link: hand-verification for this same task found a \
         Session built at the wrong rate for its transport still reached Connected (in 9.0s) - \
         the overture that gets a session to Connected never decodes anything. This step builds \
         the originate end at a rate that does not match the real device while the answer end is \
         built correctly, shares one CpalTransport between them (--acoustic, so there is a \
         genuine far end to decode against), and the pass/fail criterion is whether a real \
         payload round-trips - never the state alone."
    );
    let transport2 = match CpalTransport::new(true) {
        Ok(t) => t,
        Err(e) => {
            println!("CpalTransport::new failed: {e}");
            return;
        }
    };
    let real_rate2 = transport2.sample_rate();
    let wrong_rate = if real_rate2 == 44100 { 48000 } else { 44100 };
    println!(
        "real negotiated rate = {real_rate2}, deliberately building the ORIGINATE Session at \
         {wrong_rate} instead of using transport2.config_for (which would make this mistake \
         impossible) - the ANSWER Session is built correctly, at the transport's own real rate, \
         so the two ends genuinely disagree about the clock rather than sharing the same wrong \
         one. An earlier version of this demonstration built BOTH ends at the same wrong rate: \
         since they shared one real device and one wrong assumption, the mismatch cancelled out \
         symmetrically and the payload round-tripped anyway (verified by hand - see the task \
         report), which is not the bug the brief describes. That bug is asymmetric: one \
         genuinely misconfigured end talking to a correctly-configured far end."
    );
    let wrong_originate_cfg = Config {
        sample_rate: wrong_rate,
        role: Role::Originate,
        duplex: Duplex::HalfPingPong,
    };
    let wrong_answer_cfg = transport2.config_for(Role::Answer, Duplex::HalfPingPong);
    let mut wrong_originate = Session::new(wrong_originate_cfg);
    let mut wrong_answer = Session::new(wrong_answer_cfg);
    let wrong_start = Instant::now();
    wrong_originate.dial("5551234567");
    wrong_answer.answer();
    let mut wrong_ends = [wrong_originate, wrong_answer];
    let mut wrong_transport = transport2;
    // Paced at the REAL device rate - that is what is actually arriving,
    // regardless of what rate the Sessions above were built at.
    let wrong_period = pace(real_rate2);
    let mut wrong_both_connected_at = None;
    let deadline2 = Duration::from_secs(20);
    let (mut wrong_in_blocks, mut wrong_out_blocks, mut wrong_dropped) = (0usize, 0usize, 0u64);
    while wrong_start.elapsed() < deadline2 {
        if run_and_accumulate(
            &mut wrong_transport,
            &mut wrong_ends,
            &mut wrong_in_blocks,
            &mut wrong_out_blocks,
            &mut wrong_dropped,
        )
        .is_err()
        {
            break;
        }
        if wrong_both_connected_at.is_none()
            && wrong_ends[0].state() == SessionState::Connected
            && wrong_ends[1].state() == SessionState::Connected
        {
            wrong_both_connected_at = Some(wrong_start.elapsed());
            break;
        }
        std::thread::sleep(wrong_period);
    }
    println!(
        "asymmetric mismatch (originate built at {wrong_rate} Hz, answer correctly at the real \
         {real_rate2} Hz): states before send: originate={:?} answer={:?} both_connected_after={:?}",
        wrong_ends[0].state(),
        wrong_ends[1].state(),
        wrong_both_connected_at
    );

    // Attempt the payload exchange regardless of whether both reported
    // Connected - Connected proves nothing, and a mismatched-rate pair
    // that never even reaches it is just as much a demonstrated failure
    // to communicate as one that reaches it and then cannot decode.
    wrong_ends[0].send(b"MISMATCH TEST");
    let wrong_send_deadline = Instant::now() + Duration::from_secs(10);
    let mut wrong_payload = None;
    while Instant::now() < wrong_send_deadline {
        if run_and_accumulate(
            &mut wrong_transport,
            &mut wrong_ends,
            &mut wrong_in_blocks,
            &mut wrong_out_blocks,
            &mut wrong_dropped,
        )
        .is_err()
        {
            break;
        }
        let got = wrong_ends[1].receive();
        if !got.is_empty() {
            wrong_payload = Some(got);
            break;
        }
        std::thread::sleep(wrong_period);
    }
    println!(
        "run() totals: input_blocks={wrong_in_blocks} output_blocks={wrong_out_blocks} \
         input_samples_dropped={wrong_dropped}"
    );
    match wrong_payload {
        Some(bytes) if bytes == b"MISMATCH TEST" => println!(
            "UNEXPECTED: payload round-tripped correctly ({:?}) despite the {wrong_rate}/{real_rate2} \
             Hz mismatch - this run did not reproduce the bug the brief warns about",
            String::from_utf8_lossy(&bytes)
        ),
        Some(bytes) => println!(
            "payload arrived but corrupted under the rate mismatch: {:?} - demonstrates the bug \
             (silent corruption, not a clean failure)",
            String::from_utf8_lossy(&bytes)
        ),
        None => println!(
            "payload did NOT round-trip under the {wrong_rate} Hz Session / {real_rate2} Hz device \
             mismatch, as expected - this demonstrates exactly the bug the brief warns about: a rate \
             mismatch breaks decoding, not just the state machine"
        ),
    }
    drop(wrong_transport);
    drop(wrong_ends);

    println!("\n=== step 4: two sessions sharing one CpalTransport (--acoustic path), correctly built via config_for ===");
    let mut shared = match CpalTransport::new(true) {
        Ok(t) => t,
        Err(e) => {
            println!("CpalTransport::new(true) failed: {e}");
            return;
        }
    };
    let shared_rate = shared.sample_rate();
    println!(
        "shared transport sample_rate = {shared_rate}, is_acoustic = {}",
        shared.is_acoustic()
    );
    let orig_cfg = shared.config_for(Role::Originate, Duplex::HalfPingPong);
    let ans_cfg = shared.config_for(Role::Answer, Duplex::HalfPingPong);
    let mut originate = Session::new(orig_cfg);
    let mut answer = Session::new(ans_cfg);
    let shared_start = Instant::now();
    originate.dial("5551234567");
    answer.answer();
    let mut ends2 = [originate, answer];
    let shared_period = pace(shared_rate);
    let mut both_connected_at = None;
    let deadline3 = Duration::from_secs(20);
    let (mut shared_in_blocks, mut shared_out_blocks, mut shared_dropped) = (0usize, 0usize, 0u64);
    while shared_start.elapsed() < deadline3 {
        if let Err(e) = run_and_accumulate(
            &mut shared,
            &mut ends2,
            &mut shared_in_blocks,
            &mut shared_out_blocks,
            &mut shared_dropped,
        ) {
            println!("shared run() failed: {e}");
            break;
        }
        if both_connected_at.is_none()
            && ends2[0].state() == SessionState::Connected
            && ends2[1].state() == SessionState::Connected
        {
            both_connected_at = Some(shared_start.elapsed());
            println!(
                "both connected after {:?} of real wall-clock time",
                both_connected_at.unwrap()
            );
            break;
        }
        std::thread::sleep(shared_period);
    }
    println!(
        "final states before send: originate={:?} answer={:?}",
        ends2[0].state(),
        ends2[1].state()
    );

    if both_connected_at.is_some() {
        println!("settling 300ms, then originate sends a payload over the shared loopback...");
        let settle_deadline = Instant::now() + Duration::from_millis(300);
        while Instant::now() < settle_deadline {
            let _ = run_and_accumulate(
                &mut shared,
                &mut ends2,
                &mut shared_in_blocks,
                &mut shared_out_blocks,
                &mut shared_dropped,
            );
            std::thread::sleep(shared_period);
        }
        ends2[0].send(b"HELLO ACOUSTIC LOOPBACK");
        let send_deadline = Instant::now() + Duration::from_secs(5);
        let mut got_payload = None;
        while Instant::now() < send_deadline {
            if run_and_accumulate(
                &mut shared,
                &mut ends2,
                &mut shared_in_blocks,
                &mut shared_out_blocks,
                &mut shared_dropped,
            )
            .is_err()
            {
                break;
            }
            let got = ends2[1].receive();
            if !got.is_empty() {
                got_payload = Some(got);
                break;
            }
            std::thread::sleep(shared_period);
        }
        println!(
            "run() totals: input_blocks={shared_in_blocks} output_blocks={shared_out_blocks} \
             input_samples_dropped={shared_dropped}"
        );
        match got_payload {
            Some(bytes) if bytes == b"HELLO ACOUSTIC LOOPBACK" => println!(
                "PAYLOAD ROUND-TRIPPED byte-exact over real ALSA loopback: {:?}",
                String::from_utf8_lossy(&bytes)
            ),
            Some(bytes) => println!(
                "payload arrived but NOT byte-exact: {:?}",
                String::from_utf8_lossy(&bytes)
            ),
            None => println!("payload did NOT round-trip within the deadline"),
        }
    } else {
        println!("both ends never reached Connected within the deadline - no payload attempt made");
        println!(
            "run() totals: input_blocks={shared_in_blocks} output_blocks={shared_out_blocks} \
             input_samples_dropped={shared_dropped}"
        );
    }

    println!("\nReminder: this is a noiseless software loopback (snd-aloop), not a room. No speaker, no microphone, no air. A pass here proves the code path handles a real ALSA duplex stream correctly; it says nothing about whether the split-band trick survives real acoustic coupling.");
}
