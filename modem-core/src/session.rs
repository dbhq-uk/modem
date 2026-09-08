//! The session state machine: overture, modulator, demodulator, carrier
//! detector and link layer, tied into one connection.
//!
//! ```text
//! Idle -> dial() -> Dialling -> Answering (never both) -> Connected -> hangup() -> Idle
//!      -> answer() -> Answering ------------------------> Connected -> hangup() -> Idle
//! ```
//!
//! `dial` performs the overture (see `overture.rs`) and, once it completes,
//! brings up the real Bell 103 carrier - the originate end always holds the
//! turn from the moment it dials. `answer` skips the overture entirely
//! (dialling is decorative, see `overture.rs`'s own doc: "the listening end
//! always answers") and simply waits for carrier before coming up in the
//! answer band, without the turn - a real half-duplex answer machine does
//! not speak until it is invited to.
//!
//! # One `Config`, two ends, correctly opposite bands
//!
//! A `Session` builds its own [`Tx`] and [`Rx`] from the single [`Config`]
//! it was given. That only works correctly because of Task 12's other
//! fix: `Tx` transmits in `tones(cfg.role)` and `Rx` listens in
//! `tones(cfg.role.listen())` (see `lib.rs`'s `Role` doc) - so one
//! `Config`, naming one end, drives both halves onto the correct,
//! opposite bands. Wiring two `Session`s together with two *different*
//! `Config`s (one `Role::Originate`, one `Role::Answer`) is what a real
//! two-party call looks like, and is exactly the shape this module's own
//! tests use - never one shared `Config` for both ends, which is the
//! self-consistency trap this whole task exists to close off.
//!
//! # Half duplex: silence is not the same as idle mark
//!
//! [`Tx::read`] idles on mark when it has nothing queued - that is a real
//! transmitted tone, not silence, and it is what raises carrier on a
//! listening receiver. A half-duplex end that does not hold the turn must
//! not transmit *anything*, mark included, or the far end's receiver
//! never sees a clean carrier drop and the two ends can never agree on
//! whose turn it is. `process_out` therefore fills the block with exact
//! zero silence while `Duplex::HalfPingPong` and this end lacks the turn,
//! rather than delegating to `Tx` and trusting it to be quiet.
//!
//! # Every burst needs its own acquisition preamble
//!
//! `rx.rs`'s own module doc is explicit about what a receiver needs to
//! decode from a cold start: an alternating preamble for the timing loop
//! to acquire on, then at least one character time of idle mark to put
//! the deframer back in a known state, then data - and a Gardner loop
//! **cannot** acquire lock on a constant tone, so simply idling on mark
//! for a while first does not substitute for the alternating part. This
//! session never touches `overture.rs`'s stages for that purpose - the
//! overture runs on V.21's 980/1180 Hz, entirely outside either Bell 103
//! band, so it gives a Bell 103 receiver no acquisition signal at all
//! (see `overture.rs`'s own doc on why it takes no `Role`).
//!
//! In half duplex, the far end's receiver loses lock every time this end
//! stops transmitting - carrier drops, and with it any hope of the timing
//! loop still being where it left off. So a fresh two-character `0x55`
//! preamble (the same convention `rx.rs` and `impair.rs`'s own fixtures
//! use throughout) is queued into `tx` at the *start* of every burst: once
//! when this end first gets the turn (`grant_turn`, called from both
//! `dial` - the originate end starts with the turn - and from decoding an
//! incoming [`PacketKind::Turn`]), never mid-burst. The idle-mark gap that
//! must follow it is not manufactured here; it falls out naturally from
//! ordinary pacing, because `send` only ever appends to `tx`'s queue when
//! the caller actually has something to say, and `tx` holds idle mark on
//! its own the moment its queue runs dry. Every test in this module that
//! exercises a real exchange pumps a short settle period after acquiring
//! the turn before calling `send`, for exactly this reason.
//!
//! # Two inherited constraints
//!
//! 1. [`crate::link::encode_packet`] panics on a payload over
//!    [`MAX_PAYLOAD`] bytes - [`Session::send`] chunks before calling it,
//!    never after.
//! 2. Nothing here tries to make a Gardner loop lock onto idle mark. See
//!    the acquisition section above.

use alloc::vec::Vec;

use crate::link::{encode_packet, Packet, PacketKind, PacketReader, MAX_PAYLOAD};
use crate::overture::{Overture, Stage};
use crate::rx::Rx;
use crate::tx::Tx;
use crate::{Config, Duplex};

/// Alternating training preamble queued at the start of every transmit
/// burst. Two characters, matching the convention `rx.rs` and `impair.rs`
/// already establish elsewhere in this crate (`tx.write(&[0x55, 0x55])`) -
/// `rx.rs`'s own module doc measures that one training character is
/// enough for the timing loop to acquire on and that more does not
/// improve on it.
const TRAINING_PREAMBLE: [u8; 2] = [0x55, 0x55];

/// Where a [`Session`] is in the call.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SessionState {
    Idle,
    Dialling,
    Answering,
    Connected,
}

/// One end of a call: the overture, the modulator, the demodulator, the
/// carrier detector and the packet layer, tied together and paced by
/// `process_out`/`process_in`.
///
/// Performs no I/O itself, the same discipline `Tx` and `Rx` keep -
/// `process_out` fills a caller-owned sample block and `process_in` reads
/// one, so the same `Session` runs behind a sound card, a WAV file or a
/// test.
pub struct Session {
    cfg: Config,
    state: SessionState,
    overture: Option<Overture>,
    /// The overture stage that owned the last sample `process_out`
    /// rendered, while `state == Dialling`. `None` outside dialling, and
    /// also `None` the instant the overture finishes - see `process_out`.
    overture_stage: Option<Stage>,
    tx: Option<Tx>,
    rx: Option<Rx>,
    reader: PacketReader,
    /// Payload bytes drained from completed `PacketKind::Data` packets,
    /// oldest first. `receive` hands the whole thing over and empties it.
    inbox: Vec<u8>,
    has_turn: bool,
    /// Outgoing packet sequence number. Wraps; nothing here currently
    /// checks it on receive; `PacketReader`'s CRC is what protects a
    /// corrupt frame, not sequencing.
    seq: u8,
}

impl Session {
    /// A fresh, idle session for one end of a call. `cfg.role` says which
    /// end this is - see this module's doc and `lib.rs`'s `Role` doc for
    /// how that drives both the transmitter and the receiver correctly.
    pub fn new(cfg: Config) -> Self {
        Self {
            cfg,
            state: SessionState::Idle,
            overture: None,
            overture_stage: None,
            tx: None,
            rx: None,
            reader: PacketReader::new(),
            inbox: Vec::new(),
            has_turn: false,
            seq: 0,
        }
    }

    /// Starts dialling `digits`: performs the overture, then brings up the
    /// real Bell 103 carrier. Dialling is decorative (see `overture.rs`'s
    /// own doc) - the far end always answers regardless of what is
    /// performed here.
    ///
    /// The originate end starts with the turn: a fresh preamble is queued
    /// immediately (see this module's doc on acquisition), long before
    /// `Tx` is ever read from - it simply waits, untouched, in `tx`'s
    /// queue for the whole overture, and is the first thing that goes out
    /// once `Connected` begins reading from `tx`.
    pub fn dial(&mut self, digits: &str) {
        self.overture = Some(Overture::new(digits));
        self.overture_stage = None;
        self.tx = Some(Tx::new(self.cfg));
        self.rx = Some(Rx::new(self.cfg));
        self.reader = PacketReader::new();
        self.inbox.clear();
        self.seq = 0;
        self.state = SessionState::Dialling;
        self.grant_turn();
    }

    /// Starts answering: waits for carrier, then comes up in the answer
    /// band. No overture plays on this end - see this module's doc.
    ///
    /// The answer end does not hold the turn until the originate end
    /// explicitly yields it (a [`PacketKind::Turn`] packet, decoded in
    /// [`Session::process_in`]), so `process_out` transmits silence here
    /// until that happens.
    pub fn answer(&mut self) {
        self.overture = None;
        self.overture_stage = None;
        self.tx = Some(Tx::new(self.cfg));
        self.rx = Some(Rx::new(self.cfg));
        self.reader = PacketReader::new();
        self.inbox.clear();
        self.seq = 0;
        self.has_turn = false;
        self.state = SessionState::Answering;
    }

    /// Ends the call and returns to `Idle`. A subsequent `dial` or
    /// `answer` builds an entirely fresh `Tx`/`Rx` pair, so no timing
    /// lock, queued bits or packet-reassembly state survives a hangup.
    pub fn hangup(&mut self) {
        self.state = SessionState::Idle;
        self.overture = None;
        self.overture_stage = None;
        self.tx = None;
        self.rx = None;
        self.inbox.clear();
        self.has_turn = false;
    }

    /// Fills `out` with whatever this end should be transmitting right
    /// now: silence when idle or answering-but-not-yet-connected, the
    /// overture's next block while dialling, or modulated Bell 103 while
    /// connected.
    ///
    /// While `Duplex::HalfPingPong` and this end does not hold the turn,
    /// Connected still transmits real content for as long as `tx` has
    /// bits already queued (`tx.pending()`) - this is what lets a queued
    /// `PacketKind::Turn` packet (see [`Session::yield_turn`]) actually
    /// reach the wire after `has_turn` has already been cleared, rather
    /// than being silently stranded in `tx`'s own queue.
    pub fn process_out(&mut self, out: &mut [f32]) {
        match self.state {
            SessionState::Idle | SessionState::Answering => out.fill(0.0),
            SessionState::Dialling => {
                let overture = self
                    .overture
                    .as_mut()
                    .expect("Dialling state without an overture");
                let (_, stage, done) = overture.read(out, self.cfg.sample_rate);
                if done {
                    self.overture = None;
                    self.overture_stage = None;
                    self.state = SessionState::Connected;
                } else {
                    self.overture_stage = Some(stage);
                }
            }
            SessionState::Connected => {
                let tx = self.tx.as_mut().expect("Connected state without a Tx");
                let transmit = match self.cfg.duplex {
                    Duplex::Full => true,
                    Duplex::HalfPingPong => self.has_turn || tx.pending(),
                };
                if transmit {
                    tx.read(out);
                } else {
                    out.fill(0.0);
                }
            }
        }
    }

    /// Hands the receiver the next block of incoming samples and drains
    /// any completed, CRC-valid packets into the inbox (`Data`) or the
    /// turn flag (`Turn`). `Ack` is accepted on the wire but not acted on
    /// here - this task does not add retransmission (see `link.rs`'s own
    /// doc: chat characters ride without retry, deliberately).
    ///
    /// While `Answering`, also watches for carrier: the moment the
    /// receiver reports it, this end comes up in Connected, still without
    /// the turn (see `answer`'s own doc).
    pub fn process_in(&mut self, input: &[f32]) {
        let mut turn_received = false;
        let mut carrier_now = false;
        if let Some(rx) = self.rx.as_mut() {
            rx.write(input);
            let mut raw = [0u8; 256];
            loop {
                let n = rx.read(&mut raw);
                if n == 0 {
                    break;
                }
                for pkt in self.reader.push(&raw[..n]) {
                    match pkt.kind {
                        PacketKind::Data => self.inbox.extend_from_slice(&pkt.payload),
                        PacketKind::Turn => turn_received = true,
                        PacketKind::Ack => {}
                    }
                }
            }
            carrier_now = rx.carrier_detected();
        }
        if turn_received {
            self.grant_turn();
        }
        if self.state == SessionState::Answering && carrier_now {
            self.state = SessionState::Connected;
        }
    }

    /// Queues `data` for transmission, chunked to [`MAX_PAYLOAD`] bytes
    /// per packet before [`encode_packet`] ever sees it - `encode_packet`
    /// panics above that length, and chunking here, rather than trusting
    /// every caller to pre-chunk, is what keeps a call to `send` with an
    /// arbitrarily long payload from panicking instead of failing
    /// cleanly.
    ///
    /// Does nothing if this session has no active `Tx` (not dialling,
    /// answering or connected). Callers are responsible for checking
    /// [`Session::has_turn`] before sending on a `Duplex::HalfPingPong`
    /// link - `send` itself does not gate on it, matching `yield_turn`'s
    /// own contract of writing to `tx` unconditionally.
    pub fn send(&mut self, data: &[u8]) {
        if self.tx.is_none() {
            return;
        }
        for chunk in data.chunks(MAX_PAYLOAD) {
            let seq = self.next_seq();
            let pkt = Packet {
                seq,
                kind: PacketKind::Data,
                payload: chunk.to_vec(),
            };
            let encoded = encode_packet(&pkt);
            self.tx.as_mut().expect("checked above").write(&encoded);
        }
    }

    /// Drains and returns every payload byte received so far, oldest
    /// first. Returns an empty `Vec` if nothing is waiting.
    pub fn receive(&mut self) -> Vec<u8> {
        core::mem::take(&mut self.inbox)
    }

    /// Hands the turn to the far end: queues a [`PacketKind::Turn`] packet
    /// and clears the local flag. The packet is queued into `tx`
    /// regardless of `has_turn`'s prior value, and `process_out` keeps
    /// transmitting until `tx.pending()` goes false even after `has_turn`
    /// is cleared here - see `process_out`'s own doc - so the token
    /// itself is not stranded the instant this returns.
    pub fn yield_turn(&mut self) {
        if let Some(tx) = self.tx.as_mut() {
            let seq = self.seq;
            self.seq = self.seq.wrapping_add(1);
            let pkt = Packet {
                seq,
                kind: PacketKind::Turn,
                payload: Vec::new(),
            };
            tx.write(&encode_packet(&pkt));
        }
        self.has_turn = false;
    }

    /// Whether this end currently holds permission to transmit real data
    /// under `Duplex::HalfPingPong`. Always `true` under `Duplex::Full`'s
    /// own transmit condition in `process_out`, but this flag itself is
    /// not duplex-aware - it simply records the last turn token seen.
    pub fn has_turn(&self) -> bool {
        self.has_turn
    }

    /// The call state.
    pub fn state(&self) -> SessionState {
        self.state
    }

    /// The overture stage that owned the most recently rendered sample,
    /// while dialling. `None` outside `SessionState::Dialling`.
    pub fn stage(&self) -> Option<Stage> {
        self.overture_stage
    }

    /// Whether the receiver currently reports in-band energy above the
    /// noise floor. `false` whenever this session has no active `Rx`
    /// (`Idle`, or between construction and the first `dial`/`answer`).
    /// Delegates to the real `Rx` rather than tracking a session-local
    /// copy, so this can never drift from what the receiver actually
    /// measured - see `rx.rs`'s own caveat that carrier present is not
    /// the same claim as the timing loop being locked.
    pub fn carrier_detected(&self) -> bool {
        self.rx.as_ref().is_some_and(Rx::carrier_detected)
    }

    /// Grants this end the turn and queues a fresh acquisition preamble -
    /// called both when the originate end starts with the turn (`dial`)
    /// and whenever a `PacketKind::Turn` packet is decoded
    /// (`process_in`). See this module's doc on why every burst needs its
    /// own preamble, not just the first one.
    fn grant_turn(&mut self) {
        self.has_turn = true;
        if let Some(tx) = self.tx.as_mut() {
            tx.write(&TRAINING_PREAMBLE);
        }
    }

    fn next_seq(&mut self) -> u8 {
        let seq = self.seq;
        self.seq = self.seq.wrapping_add(1);
        seq
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Role;
    use alloc::vec;

    fn cfg(role: Role) -> Config {
        Config {
            sample_rate: 8000,
            role,
            duplex: Duplex::HalfPingPong,
        }
    }

    /// Wires two sessions' audio together for one block: each one's
    /// `process_out` becomes the other's `process_in` input. Both
    /// directions every call, regardless of duplex mode - the half-duplex
    /// discipline lives in what `process_out` actually emits (see its own
    /// doc), not in this harness.
    const BLOCK: usize = 256;

    fn pump(a: &mut Session, b: &mut Session) {
        let mut oa = vec![0.0f32; BLOCK];
        let mut ob = vec![0.0f32; BLOCK];
        a.process_out(&mut oa);
        b.process_out(&mut ob);
        a.process_in(&ob);
        b.process_in(&oa);
    }

    /// Generous upper bound on pump iterations for any single wait in
    /// this file's tests - about 128 s of simulated audio at 8 kHz.
    /// `overture.rs`'s own bound keeps the whole performance under 20 s,
    /// so this leaves ample margin without risking a genuine hang running
    /// forever if something is actually broken.
    const MAX_ITERS: usize = 4000;

    /// Pumps both sessions until both report `Connected`, or panics.
    fn connect(originate: &mut Session, answer: &mut Session) {
        for _ in 0..MAX_ITERS {
            pump(originate, answer);
            if originate.state() == SessionState::Connected
                && answer.state() == SessionState::Connected
            {
                return;
            }
        }
        panic!(
            "sessions never both reached Connected: originate {:?}, answer {:?}",
            originate.state(),
            answer.state()
        );
    }

    /// A short settle: pumps a handful of blocks with nothing queued, so
    /// a just-granted turn's acquisition preamble (and the idle-mark gap
    /// that must follow it - see this module's doc) has genuinely drained
    /// before the caller queues real data. Every fixture elsewhere in this
    /// crate that exercises acquisition does the equivalent by hand
    /// (settle, then preamble, then a gap, then data); this is that same
    /// shape at the session layer.
    fn settle(a: &mut Session, b: &mut Session) {
        for _ in 0..10 {
            pump(a, b);
        }
    }

    #[test]
    fn session_starts_idle() {
        let s = Session::new(cfg(Role::Originate));
        assert_eq!(s.state(), SessionState::Idle);
        assert_eq!(s.stage(), None);
        assert!(!s.has_turn());
        assert!(!s.carrier_detected());
    }

    /// Drives `dial` one sample at a time (matching `overture.rs`'s own
    /// `render_all`) so no stage boundary can be smeared across a larger
    /// block, and collects the distinct stage sequence actually observed -
    /// not just the final `state()`.
    ///
    /// Own mutation target (Mutation 6): a `stage()` that ignored the
    /// overture's real progress and returned a constant would still leave
    /// `state()` reaching `Connected` correctly - that transition is
    /// driven by `Overture::read`'s own `done` flag, not by anything
    /// `stage()` reports - so a test that only checked `state()` at the
    /// end could not tell a working `stage()` from a broken one. See the
    /// task report.
    #[test]
    fn dial_passes_through_every_overture_stage_and_reaches_connected() {
        let mut s = Session::new(cfg(Role::Originate));
        s.dial("1");

        let mut seen: Vec<Stage> = Vec::new();
        let mut buf = [0.0f32; 1];
        let mut iters = 0usize;
        while s.state() != SessionState::Connected {
            s.process_out(&mut buf);
            if let Some(stage) = s.stage() {
                if seen.last() != Some(&stage) {
                    seen.push(stage);
                }
            }
            iters += 1;
            assert!(
                iters < 8000 * 30,
                "dial did not reach Connected within 30 s of audio"
            );
        }

        assert_eq!(
            seen,
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
            ],
            "dial did not pass through every overture stage in order"
        );
        assert_eq!(s.state(), SessionState::Connected);
    }

    /// The test the Role fix exists for. Two sessions built from two
    /// different, correctly-configured `Config`s - never one shared
    /// `Config` for both ends, which is exactly the shape that cannot
    /// exercise this bug (see this module's own doc and `lib.rs`'s `Role`
    /// doc). Mutation 1 (revert the Role fix) fails this test directly -
    /// see the task report for the actual failure it produces.
    ///
    /// Checks content, not aggregates: `receive()` is compared against the
    /// literal bytes sent, never a length or a `contains`.
    #[test]
    fn two_sessions_exchange_data_in_both_directions() {
        let mut originate = Session::new(cfg(Role::Originate));
        let mut answer = Session::new(cfg(Role::Answer));

        originate.dial("1");
        answer.answer();
        connect(&mut originate, &mut answer);
        settle(&mut originate, &mut answer);

        assert!(originate.has_turn(), "originate must start with the turn");
        originate.send(b"HELLO ANSWER");
        for _ in 0..500 {
            pump(&mut originate, &mut answer);
        }
        assert_eq!(answer.receive(), b"HELLO ANSWER");

        originate.yield_turn();
        let mut turned = false;
        for _ in 0..MAX_ITERS {
            pump(&mut originate, &mut answer);
            if answer.has_turn() {
                turned = true;
                break;
            }
        }
        assert!(turned, "answer never received the turn");
        settle(&mut originate, &mut answer);

        answer.send(b"HELLO ORIGINATE");
        for _ in 0..500 {
            pump(&mut originate, &mut answer);
        }
        assert_eq!(originate.receive(), b"HELLO ORIGINATE");
    }

    /// Half duplex, checked at the sample level rather than through
    /// `has_turn()` alone. `has_turn` is a bool - exactly the kind of
    /// aggregate the brief warns cannot detect the defect it exists to
    /// catch: Mutation 2 (`process_out` ignoring the turn) still reports
    /// `has_turn() == false` correctly on the end that lacks it, so only
    /// inspecting the actual samples `process_out` writes catches it.
    #[test]
    fn half_duplex_end_without_turn_transmits_exact_silence() {
        let mut originate = Session::new(cfg(Role::Originate));
        let mut answer = Session::new(cfg(Role::Answer));
        originate.dial("1");
        answer.answer();
        connect(&mut originate, &mut answer);

        assert!(!answer.has_turn(), "answer must not start with the turn");
        // Pre-filled with a non-zero value so an untouched buffer would
        // also be caught, not only one actively overwritten with tone.
        let mut out = vec![1.0f32; BLOCK];
        answer.process_out(&mut out);
        assert!(
            out.iter().all(|&x| x == 0.0),
            "answer transmitted while it did not hold the turn"
        );

        originate.yield_turn();
        let mut turned = false;
        for _ in 0..MAX_ITERS {
            pump(&mut originate, &mut answer);
            if answer.has_turn() {
                turned = true;
                break;
            }
        }
        assert!(turned, "answer never received the turn");

        let mut out2 = vec![0.0f32; BLOCK];
        answer.process_out(&mut out2);
        assert!(
            out2.iter().any(|&x| x != 0.0),
            "answer transmitted silence even after acquiring the turn"
        );
    }

    #[test]
    fn hangup_returns_to_idle_and_a_subsequent_dial_works() {
        let mut s = Session::new(cfg(Role::Originate));
        s.dial("1");
        let mut buf = vec![0.0f32; BLOCK];
        s.process_out(&mut buf);
        assert_eq!(s.state(), SessionState::Dialling);

        s.hangup();
        assert_eq!(s.state(), SessionState::Idle);
        assert_eq!(s.stage(), None);
        assert!(!s.has_turn());

        s.dial("2");
        let mut buf1 = [0.0f32; 1];
        let mut iters = 0usize;
        while s.state() != SessionState::Connected {
            s.process_out(&mut buf1);
            iters += 1;
            assert!(iters < 8000 * 30, "second dial never reached Connected");
        }
        assert_eq!(s.state(), SessionState::Connected);
    }

    /// `encode_packet` panics above `MAX_PAYLOAD` bytes; `send` must
    /// chunk before it ever gets there. Verified through content, not
    /// just the absence of a panic: a real two-session exchange, with the
    /// far end reassembling the chunks back into the exact original
    /// bytes. Mutation 4 (send passing the whole payload straight to
    /// `encode_packet`) panics inside this test rather than failing an
    /// assertion - see the task report.
    #[test]
    fn send_of_an_oversized_payload_is_chunked_not_a_panic() {
        let mut originate = Session::new(cfg(Role::Originate));
        let mut answer = Session::new(cfg(Role::Answer));
        originate.dial("1");
        answer.answer();
        connect(&mut originate, &mut answer);
        settle(&mut originate, &mut answer);

        let payload: Vec<u8> = (0..600u32).map(|i| (i % 256) as u8).collect();
        assert!(
            payload.len() > 2 * MAX_PAYLOAD,
            "payload must actually require more than one chunk"
        );
        originate.send(&payload);

        for _ in 0..2000 {
            pump(&mut originate, &mut answer);
        }
        assert_eq!(
            answer.receive(),
            payload,
            "oversized payload did not reassemble byte-exact after chunking"
        );
    }

    #[test]
    fn carrier_detected_reflects_the_receivers_state() {
        let mut originate = Session::new(cfg(Role::Originate));
        let mut answer = Session::new(cfg(Role::Answer));
        assert!(
            !answer.carrier_detected(),
            "carrier detected before any call started"
        );

        originate.dial("1");
        answer.answer();
        connect(&mut originate, &mut answer);
        assert!(
            answer.carrier_detected(),
            "no carrier once originate is transmitting"
        );
    }
}
