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
//! opposite bands.
//!
//! # The role follows the command, not the `Config` it started with (Task 19)
//!
//! Before Task 19, `Config.role` was fixed at construction and never
//! touched again: `dial` and `answer` rebuilt `Tx`/`idle_tx`/`Rx` from
//! `self.cfg`, but neither ever changed what `self.cfg.role` actually
//! said. Two ends built from the *same* `Config` - exactly what happens
//! when the same binary, with the same flags, runs on two machines -
//! were therefore always the same role, always transmitting in the same
//! band and always listening in the same band, and could never hear each
//! other. Every test in this module built its two ends from two
//! *different* `Config`s (one naming `Role::Originate`, one naming
//! `Role::Answer`), which is the only shape that could ever work under
//! the old design - so the defect stayed invisible until two genuinely
//! independent, identically-configured ends had to talk for real. That
//! is exactly the shape `modem-tui`'s `--single --acoustic` on two
//! separate machines with no way to choose a role produced, and it is
//! why the two-machine case - the actual product - could not work.
//!
//! The fix is the period-correct one, matching a real Hayes modem: `ATD`
//! put you in originate mode and `ATA` put you in answer mode, not a
//! switch set beforehand. `dial` now sets `self.cfg.role =
//! Role::Originate` and `answer` sets `self.cfg.role = Role::Answer`,
//! *before* rebuilding `Tx`, `idle_tx` and `Rx` from that same
//! `self.cfg` - so both ends can be built from one identical `Config`
//! and still land in opposite bands, purely because one of them dialled
//! and the other answered. `Config.role` is now only ever the band an
//! idle session - one that has never dialled or answered - starts in;
//! [`Session::role`] is the one true answer to "which end is this,
//! right now", and any caller that wants to display or reason about the
//! current role (a title bar, a status line) must read it from there,
//! never from a `Config` snapshotted at some earlier point - see
//! `role`'s own doc for why a stale copy silently goes wrong the moment
//! `dial`/`answer` is called.
//!
//! # Half duplex: silence is not the same as idle mark
//!
//! **This was wrong, and it tore every half-duplex call down on the
//! first turn hand-over.** An earlier version of this module reasoned
//! that a half-duplex end without the turn "must not transmit
//! *anything*, mark included, or the far end's receiver never sees a
//! clean carrier drop" - and filled the block with exact zero silence
//! while `Duplex::HalfPingPong` and this end lacked the turn. That is
//! backwards from how a real modem behaves, and it is fatal: a real
//! modem holds its carrier up for the *entire call* and sits on idle
//! mark - a genuine transmitted tone - whenever it has nothing to send;
//! ping-pong governs who sends *data*, not who transmits at all. Zero
//! silence, by contrast, is exactly what a *dropped call* looks like to
//! the far end's receiver ([`crate::carrier::CarrierDetector`] reads it
//! as energy below the noise floor), so the very first hand-over -
//! `dial`'s originate end yields the turn the moment it has said
//! anything at all - made the answer end's receiver declare carrier
//! loss on the originate direction, [`crate::at::AtProcessor::poll_carrier`]
//! correctly hangs up on exactly that signal, and the call was torn down
//! before a single real exchange completed. Traced end to end on 8 Sep
//! 2026 with two [`Session`]s wired together: connect, yield the turn,
//! both ends `Idle` within a second, the message never arrives. This
//! also explains two symptoms that looked separate and were not: the
//! `CARRIER` lamp flickering off on an otherwise healthy call, and
//! `CONNECT 300` never arriving at the originate end at all (it never
//! gets to see the answer end's carrier stay up long enough to report
//! it).
//!
//! The fix is the period-accurate one: `process_out` now transmits
//! continuous idle mark from this end's **own** [`Tx`] - the same tone
//! [`Tx::read`] already generates whenever its bit queue runs dry, so a
//! turn hand-over is inaudible from the wire's point of view - instead
//! of the real `tx`, which cannot be reused directly for this: `send`
//! does not gate on `has_turn` (see its own doc), so anything a caller
//! incorrectly queued before actually holding the turn already sits in
//! `tx`'s bit queue, and reading from `tx` here would drain and
//! transmit it early. A second, dedicated `Tx` - `idle_tx`, built from
//! the same [`Config`] alongside the real one and never written to, so
//! its own bit queue is permanently empty - is used instead: reading
//! from it can only ever produce this end's own idle mark, never real
//! data, regardless of what a misbehaving caller has queued into `tx`.
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
//! This is proven, not assumed: `acquisition_preamble_is_needed_under_a_
//! real_clock_offset` deletes `grant_turn`'s preamble and drives a real
//! exchange under a genuine 2% clock offset (`sample_rate` 7840 on one
//! end, 8000 on the other - `rx.rs`'s own convention for simulating two
//! sound cards that never agree). Round 1 review found this needed real
//! care: every other test in this module runs both ends off the identical
//! `sample_rate`, where a free-running symbol counter is already exact
//! and cannot show the preamble mattering at all, and even the first
//! clock-offset scenario tried (8160 Hz, the module's own settle
//! convention) happened to land in a winning phase band and decoded
//! clean with no preamble whatsoever - the same phase-lottery shape
//! `rx.rs`'s own sub-symbol offset sweep names. The scenario this test
//! actually uses was picked from a small sweep specifically because it
//! measured failing without the preamble, not because it was the first
//! one tried.
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
use crate::{Config, Duplex, Role};

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
    /// A second transmitter, built from the same [`Config`] as `tx` and
    /// never written to, so its bit queue is permanently empty and
    /// reading it can only ever produce this end's own idle mark. Used
    /// by `process_out` under `Duplex::HalfPingPong` while this end does
    /// not hold the turn - see this module's own doc for why `tx` itself
    /// cannot be reused for that without risking whatever a caller
    /// queued into it before actually holding the turn.
    idle_tx: Option<Tx>,
    rx: Option<Rx>,
    reader: PacketReader,
    /// Payload bytes drained from completed `PacketKind::Data` packets,
    /// oldest first. `receive` hands the whole thing over and empties it.
    inbox: Vec<u8>,
    has_turn: bool,
    /// Set by `yield_turn`, cleared by `grant_turn` and `hangup`. Narrows
    /// `process_out`'s `tx.pending()` bypass (see its own doc) to exactly
    /// the one case it exists for - flushing a just-queued `Turn` packet
    /// after `has_turn` is cleared - rather than to anything at all that
    /// happens to be sitting in `tx`'s queue, which a `send` called before
    /// this end holds the turn would also satisfy. See `process_out`'s own
    /// doc for the defect this closes.
    yielding: bool,
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
            idle_tx: None,
            rx: None,
            reader: PacketReader::new(),
            inbox: Vec::new(),
            has_turn: false,
            yielding: false,
            seq: 0,
        }
    }

    /// Starts dialling `digits`: performs the overture, then brings up the
    /// real Bell 103 carrier. Dialling is decorative (see `overture.rs`'s
    /// own doc) - the far end always answers regardless of what is
    /// performed here.
    ///
    /// Sets this session's role to [`Role::Originate`] *first*, then
    /// rebuilds `Tx`, `idle_tx` and `Rx` from that role (see this
    /// module's own doc, "The role follows the command") - so calling
    /// `dial` always puts this end in the originate band, even on a
    /// session built from a `Config` whose `role` said `Role::Answer`.
    /// This is what lets two ends built from one identical `Config`
    /// still land in opposite bands: whichever one dials becomes the
    /// originate end, full stop, regardless of what either `Config`
    /// happened to say beforehand.
    ///
    /// The originate end starts with the turn: a fresh preamble is queued
    /// immediately (see this module's doc on acquisition), long before
    /// `Tx` is ever read from - it simply waits, untouched, in `tx`'s
    /// queue for the whole overture, and is the first thing that goes out
    /// once `Connected` begins reading from `tx`.
    pub fn dial(&mut self, digits: &str) {
        self.cfg.role = Role::Originate;
        self.overture = Some(Overture::new(digits));
        self.overture_stage = None;
        self.tx = Some(Tx::new(self.cfg));
        self.idle_tx = Some(Tx::new(self.cfg));
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
    /// Sets this session's role to [`Role::Answer`] *first*, then
    /// rebuilds `Tx`, `idle_tx` and `Rx` from that role - the same fix
    /// `dial` applies for `Role::Originate` (see this module's own doc,
    /// "The role follows the command"), and for the identical reason: a
    /// session built from a `Config` that still said `Role::Originate`
    /// must answer in the answer band regardless, or two identically-
    /// configured ends can never hear each other.
    ///
    /// The answer end does not hold the turn until the originate end
    /// explicitly yields it (a [`PacketKind::Turn`] packet, decoded in
    /// [`Session::process_in`]), so `process_out` transmits silence here
    /// until that happens.
    pub fn answer(&mut self) {
        self.cfg.role = Role::Answer;
        self.overture = None;
        self.overture_stage = None;
        self.tx = Some(Tx::new(self.cfg));
        self.idle_tx = Some(Tx::new(self.cfg));
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
        self.idle_tx = None;
        self.rx = None;
        self.inbox.clear();
        self.has_turn = false;
        self.yielding = false;
    }

    /// Fills `out` with whatever this end should be transmitting right
    /// now: silence when idle or answering-but-not-yet-connected, the
    /// overture's next block while dialling, or modulated Bell 103 while
    /// connected.
    ///
    /// While `Duplex::HalfPingPong` and this end does not hold the turn,
    /// Connected transmits continuous idle mark from `idle_tx` - not
    /// silence, see this module's own doc - with exactly one narrow
    /// exception: immediately after `yield_turn` clears `has_turn`, `tx`
    /// still has that call's own `Turn` packet queued, and `process_out`
    /// keeps draining the *real* `tx` (via the `yielding` flag, cleared
    /// the moment `tx` runs dry) so the token is not silently stranded.
    /// That bypass covers only the packet `yield_turn` itself just
    /// queued - it is not a general "transmit whatever `tx` happens to be
    /// holding" rule. Round 1 review found the difference matters: a
    /// bypass keyed on `tx.pending()` alone also let a `send` called
    /// before this end held the turn - not just `yield_turn`'s own token -
    /// straight through, which put both ends on the wire at once on the
    /// very next `process_out` call. See [`Session::send`]'s own doc: it
    /// still does not gate on `has_turn`, so this is the only thing
    /// standing between a caller mistake and a live collision - and it is
    /// exactly why the idle path below reads from `idle_tx`, which that
    /// same caller mistake can never reach, rather than from `tx` itself.
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
                let transmit = match self.cfg.duplex {
                    Duplex::Full => true,
                    Duplex::HalfPingPong => {
                        self.has_turn
                            || (self.yielding
                                && self
                                    .tx
                                    .as_ref()
                                    .expect("Connected state without a Tx")
                                    .pending())
                    }
                };
                if transmit {
                    let tx = self.tx.as_mut().expect("Connected state without a Tx");
                    tx.read(out);
                    if self.yielding && !tx.pending() {
                        self.yielding = false;
                    }
                } else {
                    // Half duplex and this end does not hold the turn:
                    // continuous idle mark, never silence - see this
                    // module's own doc for the defect this closes.
                    // `idle_tx` is never written to, so this can only
                    // ever produce this end's own mark tone, regardless
                    // of whatever a caller may have incorrectly queued
                    // into the real `tx` before actually holding the
                    // turn.
                    self.idle_tx
                        .as_mut()
                        .expect("Connected state without an idle Tx")
                        .read(out);
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
    /// regardless of `has_turn`'s prior value, and sets `yielding` so
    /// `process_out` keeps transmitting until that specific packet has
    /// actually gone out (`tx.pending()` goes false) even after
    /// `has_turn` is cleared here - see `process_out`'s own doc for why
    /// this is narrower than "transmit until `tx` is empty for any
    /// reason".
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
        self.yielding = true;
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

    /// Which end of the call this session currently is. Not necessarily
    /// the role the `Config` passed to [`Session::new`] named:
    /// [`Session::dial`] and [`Session::answer`] both set this before
    /// rebuilding `Tx`/`idle_tx`/`Rx` (see this module's own doc, "The
    /// role follows the command"), so this is the one place to read
    /// "which end is this, right now" from. A caller that instead keeps
    /// its own earlier copy of the `Config` this session was built from -
    /// a title bar, a status line - will silently go stale the moment
    /// either method is called.
    pub fn role(&self) -> Role {
        self.cfg.role
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
        self.yielding = false;
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
    use crate::nco::goertzel;
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

    /// The headline test Task 19 exists for. Both ends here are built
    /// from the exact same `Config` - the shape that is actually broken
    /// today: `modem --single --acoustic` on two separate machines, with
    /// no way to say which one is which, hands both ends `Role::Originate`
    /// by construction. If `dial`/`answer` did not each set *this* end's
    /// own role, both sessions above would stay `Role::Originate`, both
    /// would transmit on 1270/1070 and listen on 2225/2025, and neither
    /// could ever hear the other - which is exactly the defect the brief
    /// names.
    ///
    /// Two ends both reaching `Connected` proves nothing here - that is
    /// exactly what happens today while they are deaf to each other (see
    /// this module's own doc on half duplex for the established caution
    /// against trusting `SessionState` alone) - so this is asserted on
    /// real payload bytes actually crossing in both directions, the same
    /// shape `two_sessions_exchange_data_in_both_directions` above uses,
    /// just built from one `Config` instead of two.
    ///
    /// Mutation proof target: deleting `answer`'s `self.cfg.role =
    /// Role::Answer` line leaves `answer` at `Role::Originate` - both
    /// ends then share a band and this test's `connect` call hangs until
    /// `MAX_ITERS` and panics. See the task report for the exact output.
    #[test]
    fn two_sessions_built_from_the_same_config_connect_and_exchange_data_once_one_dials_and_the_other_answers(
    ) {
        let shared = cfg(Role::Originate);
        let mut originate = Session::new(shared);
        let mut answer = Session::new(shared);

        originate.dial("1");
        answer.answer();
        connect(&mut originate, &mut answer);
        settle(&mut originate, &mut answer);

        assert_eq!(
            originate.role(),
            Role::Originate,
            "the end that dialled must be Originate"
        );
        assert_eq!(
            answer.role(),
            Role::Answer,
            "the end that answered must be Answer, even though it started from the same \
             Config as the end that dialled"
        );

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

    /// `dial` and `answer` must each put *this* end's transmitter in the
    /// correct band, proven the way this module insists on: the recovered
    /// tone frequency of what `process_out` actually renders, never the
    /// `Role` enum this test itself just read back. Each session is
    /// deliberately built from a `Config` naming the *wrong* role for
    /// what it is about to do (dial from a `Role::Answer` config, answer
    /// from a `Role::Originate` one) - a session that already happened to
    /// agree with its own destination role would pass this test even if
    /// `dial`/`answer` never touched `self.cfg.role` at all, since
    /// `Tx::new`/`Rx::new` would pick the right band from the unmodified
    /// `Config` by coincidence.
    ///
    /// Mutation proof target: deleting `dial`'s `self.cfg.role =
    /// Role::Originate` line leaves this end transmitting the answer
    /// band's mark tone it was constructed with instead - see the task
    /// report for the exact failure.
    #[test]
    fn dial_transmits_in_the_originate_band_and_answer_in_the_answer_band_regardless_of_the_config_they_started_with(
    ) {
        // dial(), from a Config that named Role::Answer.
        let mut dialler = Session::new(cfg(Role::Answer));
        dialler.dial("1");
        let mut warmup = [0.0f32; 1];
        let mut iters = 0usize;
        while dialler.state() != SessionState::Connected {
            dialler.process_out(&mut warmup);
            iters += 1;
            assert!(
                iters < 8000 * 30,
                "dial did not reach Connected within 30 s of audio"
            );
        }
        // Past the two-character acquisition preamble (about 534 samples
        // at 8 kHz) and settled onto continuous idle mark, so this block
        // is not a mix of preamble and idle mark - see this module's own
        // doc on why every burst needs its own preamble.
        let mut drain = vec![0.0f32; 4096];
        dialler.process_out(&mut drain);
        let mut out = vec![0.0f32; 4096];
        dialler.process_out(&mut out);
        let s64: Vec<f64> = out.iter().map(|&x| x as f64).collect();
        let originate_mark = goertzel(&s64, 1270.0, 8000.0);
        let answer_mark = goertzel(&s64, 2225.0, 8000.0);
        assert!(
            originate_mark >= 0.9,
            "dial() did not transmit the originate band's mark tone (1270 Hz): {originate_mark}"
        );
        assert!(
            answer_mark <= 0.05,
            "dial() leaked the answer band's mark tone (2225 Hz) instead of transmitting its \
             own: {answer_mark}"
        );

        // answer(), from a Config that named Role::Originate. answer()
        // transmits silence, not idle mark, until it actually reaches
        // Connected (see process_out's own doc), so a real originate-band
        // tone is fed straight in to raise carrier - the same pattern
        // `answering_waits_for_carrier_and_carrier_detected_tracks_the_
        // real_receiver` above uses.
        let mut answerer = Session::new(cfg(Role::Originate));
        answerer.answer();
        let mut carrier_tx = Tx::new(cfg(Role::Originate));
        let mut carrier_tone = vec![0.0f32; 8000];
        carrier_tx.read(&mut carrier_tone);
        for chunk in carrier_tone.chunks(256) {
            answerer.process_in(chunk);
        }
        assert_eq!(
            answerer.state(),
            SessionState::Connected,
            "answer() never reached Connected after a real originate-band tone"
        );

        let mut out2 = vec![0.0f32; 4096];
        answerer.process_out(&mut out2);
        let s64_2: Vec<f64> = out2.iter().map(|&x| x as f64).collect();
        let answer_mark2 = goertzel(&s64_2, 2225.0, 8000.0);
        let originate_mark2 = goertzel(&s64_2, 1270.0, 8000.0);
        assert!(
            answer_mark2 >= 0.9,
            "answer() did not transmit the answer band's mark tone (2225 Hz): {answer_mark2}"
        );
        assert!(
            originate_mark2 <= 0.05,
            "answer() leaked the originate band's mark tone (1270 Hz) instead of transmitting \
             its own: {originate_mark2}"
        );
    }

    /// Round 1 review finding: deleting `grant_turn`'s
    /// `tx.write(&TRAINING_PREAMBLE)` left all of this module's other
    /// tests green, because every one of them runs both ends off the
    /// identical `sample_rate` - the same trap `rx.rs`'s own
    /// `loopback_tracks_a_two_percent_sample_clock_offset` names: with no
    /// real clock offset, a free-running symbol counter is already
    /// exact, so nothing here actually exercised the timing loop's
    /// acquisition at all.
    ///
    /// This drives the same 2% offset that test uses (`sample_rate` 8160
    /// on one end, 8000 on the other) through a real `Session` exchange,
    /// so the acquisition preamble this module's doc names as resolving
    /// the brief's second inherited constraint is proven at the layer
    /// that actually claims it, not only in `rx.rs`.
    #[test]
    fn acquisition_preamble_is_needed_under_a_real_clock_offset() {
        // 7840 Hz specifically, not 8160: an 8-point sweep (settle 0 or
        // 10 blocks, tx_rate in {8160, 7840, 8320, 7680}) with the
        // preamble removed found this exact combination reliably fails
        // (180 of 420 bytes recovered), while 8160/settle-10 happened to
        // land in a winning phase band and decoded clean anyway - the
        // same phase-lottery shape `rx.rs`'s own module doc names for its
        // sub-symbol offset sweep. Picking a scenario this sweep actually
        // measured failing, rather than the first one tried, is what
        // makes this a real proof and not a coincidence.
        let originate_cfg = Config {
            sample_rate: 7840,
            role: Role::Originate,
            duplex: Duplex::HalfPingPong,
        };
        let mut originate = Session::new(originate_cfg);
        let mut answer = Session::new(cfg(Role::Answer));

        originate.dial("1");
        answer.answer();
        connect(&mut originate, &mut answer);
        settle(&mut originate, &mut answer);

        let mut payload = Vec::new();
        for _ in 0..20 {
            payload.extend_from_slice(b"The quick brown fox. ");
        }
        originate.send(&payload);
        for _ in 0..3000 {
            pump(&mut originate, &mut answer);
        }
        assert_eq!(
            answer.receive(),
            payload,
            "a 2% clock offset did not decode byte-exact with the acquisition preamble in place"
        );
    }

    /// The Task 17 fix's own central test, checked at the sample level
    /// rather than through `has_turn()` alone. `has_turn` is a bool -
    /// exactly the kind of aggregate the brief warns cannot detect the
    /// defect it exists to catch: a `process_out` that ignores the turn
    /// entirely still reports `has_turn() == false` correctly on the end
    /// that lacks it, so only inspecting the actual samples `process_out`
    /// writes catches a regression here.
    ///
    /// Before this task, the end without the turn transmitted exact
    /// silence, which the far end's receiver reads as carrier loss - see
    /// this module's own doc for the call this tore down on the very
    /// first hand-over. The fix is continuous idle mark instead, checked
    /// two ways that a bare "any nonzero sample" scan cannot tell apart
    /// from a defect: RMS (a single stray spike, or a DC offset, would
    /// also read as "not all zero" but would not clear a real sine's RMS)
    /// and the recovered tone frequency being answer's *own* mark
    /// (2225 Hz), not originate's (1270 Hz) and not merely "some energy
    /// somewhere".
    ///
    /// Mutation 1 target: put the zero-fill back in `process_out` (delete
    /// the `idle_tx` read and restore `out.fill(0.0)` in the `else`
    /// branch) - the RMS assertion below fails outright. Mutation 2
    /// target: build `idle_tx` from the *far* end's role (e.g.
    /// `self.cfg.role.listen()`) instead of this end's own - `own_mark`
    /// drops near zero and `far_mark` clears 0.9 instead, failing the
    /// frequency assertions specifically while leaving RMS untouched -
    /// see the task report for the actual failing output either mutation
    /// produces.
    #[test]
    fn half_duplex_end_without_turn_transmits_continuous_idle_mark_not_silence() {
        let mut originate = Session::new(cfg(Role::Originate));
        let mut answer = Session::new(cfg(Role::Answer));
        originate.dial("1");
        answer.answer();
        connect(&mut originate, &mut answer);

        assert!(!answer.has_turn(), "answer must not start with the turn");
        let mut out = vec![0.0f32; BLOCK];
        answer.process_out(&mut out);

        let rms =
            (out.iter().map(|&x| (x as f64) * (x as f64)).sum::<f64>() / out.len() as f64).sqrt();
        assert!(
            rms > 0.3,
            "answer's idle output has implausibly low RMS ({rms}) for a full-amplitude tone - \
             a DC offset or a single spike would also satisfy a bare non-zero check"
        );

        let s64: Vec<f64> = out.iter().map(|&x| x as f64).collect();
        let own_mark = goertzel(&s64, 2225.0, 8000.0);
        let far_mark = goertzel(&s64, 1270.0, 8000.0);
        assert!(
            own_mark >= 0.9,
            "answer's idle output is not its own mark tone (2225 Hz): {own_mark}"
        );
        assert!(
            far_mark <= 0.05,
            "answer's idle output leaked the far end's mark tone (1270 Hz) instead of its own: \
             {far_mark}"
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

    /// Round 1 review finding: gating `process_out`'s bypass on
    /// `tx.pending()` alone - rather than on `yielding && tx.pending()` -
    /// let anything sitting in `tx`'s queue through, not only a just-
    /// queued `Turn` packet. `send` does not gate on `has_turn` (see its
    /// own doc - that is the caller's responsibility), so calling it
    /// before this end actually holds the turn queued real data straight
    /// into `tx`, and the old, wider condition read that queued data as
    /// license to transmit.
    ///
    /// Task 17 changed what the *correct* output looks like here: real
    /// modems never go silent while connected, so the fix reads idle
    /// output from a second, dedicated `idle_tx` that is never written to
    /// (see this module's own doc) rather than from `tx` - which means
    /// this scenario's correct output is now idle mark, not silence, and
    /// this test's own job is proving the two stay genuinely independent:
    /// the queued data must never reach the wire while answer still lacks
    /// the turn, no matter how long it waits, even though the output is
    /// no longer silent either.
    #[test]
    fn sending_without_the_turn_transmits_idle_mark_not_the_queued_data() {
        let mut originate = Session::new(cfg(Role::Originate));
        let mut answer = Session::new(cfg(Role::Answer));
        originate.dial("1");
        answer.answer();
        connect(&mut originate, &mut answer);

        assert!(!answer.has_turn(), "answer must not start with the turn");
        answer.send(b"jumping the queue");

        let mut out = vec![0.0f32; BLOCK];
        answer.process_out(&mut out);
        let s64: Vec<f64> = out.iter().map(|&x| x as f64).collect();
        let own_mark = goertzel(&s64, 2225.0, 8000.0);
        assert!(
            own_mark >= 0.9,
            "answer must still transmit its own idle mark, not the data queued before it held \
             the turn: {own_mark}"
        );

        // The queued data must never reach the wire while answer still
        // lacks the turn - a long pump, with originate never yielding.
        for _ in 0..2000 {
            pump(&mut originate, &mut answer);
        }
        assert!(
            originate.receive().is_empty(),
            "data queued before the turn was ever held leaked onto the wire"
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

    /// Round 1 review finding: the test above only ever hangs up from
    /// `Dialling`, where `tx`, `rx`, `inbox` and `has_turn` are all still
    /// at their fresh-`dial()` defaults - a `hangup` that forgot to clear
    /// any of them could pass it unnoticed. `Connected` is the state
    /// where all four actually hold something. This hangs up the answer
    /// end mid-call, with a real undrained inbox and a real `Rx` that has
    /// genuinely heard carrier, and checks each is actually reset before
    /// proving a fresh call still works end to end.
    #[test]
    fn hangup_from_connected_tears_down_everything_and_a_fresh_dial_still_works() {
        let mut originate = Session::new(cfg(Role::Originate));
        let mut answer = Session::new(cfg(Role::Answer));
        originate.dial("1");
        answer.answer();
        connect(&mut originate, &mut answer);
        settle(&mut originate, &mut answer);

        originate.send(b"before hangup");
        for _ in 0..500 {
            pump(&mut originate, &mut answer);
        }

        // Hang up the answer end while it genuinely holds something: a
        // live Tx/Rx pair with real carrier detected, and an inbox that
        // has not been drained.
        answer.hangup();
        assert_eq!(answer.state(), SessionState::Idle);
        assert_eq!(answer.stage(), None);
        assert!(!answer.has_turn());
        assert!(
            answer.receive().is_empty(),
            "hangup did not clear the inbox - a byte received before hangup leaked into a fresh call"
        );
        assert!(
            !answer.carrier_detected(),
            "hangup left carrier_detected() reporting the old Rx's state"
        );

        // A fresh call end to end, with a fresh originate too, must work
        // exactly as it would on a session that had never connected at
        // all.
        let mut originate2 = Session::new(cfg(Role::Originate));
        originate2.dial("2");
        answer.answer();
        connect(&mut originate2, &mut answer);
        settle(&mut originate2, &mut answer);

        originate2.send(b"after hangup");
        for _ in 0..500 {
            pump(&mut originate2, &mut answer);
        }
        assert_eq!(
            answer.receive(),
            b"after hangup",
            "a fresh call after hangup did not decode cleanly - stale state leaked through"
        );
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

    /// Round 1 review finding: `answer()` moving `state()` from
    /// `Answering` straight to `Connected`, `process_in`'s carrier
    /// transition being deleted outright, and `carrier_detected()`
    /// returning `self.rx.is_some()` instead of delegating to the real
    /// `Rx` all survived the full suite - `state()` is an enum and
    /// `carrier_detected()` a bool, and both are aggregates in exactly
    /// the shape Mutation 6 (own, on `stage()`) already proved this
    /// project cannot trust without a test that checks the actual
    /// transition, not just an endpoint. `carrier_detected_reflects_the_
    /// receivers_state` above only ever checks a "no Rx yet" point and a
    /// "has Rx and carrier" point - both `self.rx.is_none()` and
    /// `self.rx.is_some()` happen to agree with the correct answer at
    /// those two points, so a mutation swapping in `self.rx.is_some()`
    /// passes it unchanged.
    ///
    /// This drives the middle case those miss: a session with a real
    /// `Rx` (`answer()` has already run) that has not yet heard anything,
    /// where the correct answer and `self.rx.is_some()` disagree. Feeds a
    /// real `Tx`-generated Originate-band tone directly, rather than
    /// wiring a second `Session`, so the carrier source and the acquiring
    /// receiver are decoupled from anything else this file's other tests
    /// already establish.
    #[test]
    fn answering_waits_for_carrier_and_carrier_detected_tracks_the_real_receiver() {
        let mut answer = Session::new(cfg(Role::Answer));
        answer.answer();
        assert_eq!(
            answer.state(),
            SessionState::Answering,
            "answer() did not enter Answering"
        );
        assert!(
            !answer.carrier_detected(),
            "carrier detected immediately after answer(), before anything arrived"
        );

        // A good stretch of plain silence must not, on its own, ever look
        // like carrier or advance the state - answering only moves once
        // it actually hears something.
        let silence = vec![0.0f32; BLOCK];
        for _ in 0..50 {
            answer.process_in(&silence);
        }
        assert_eq!(
            answer.state(),
            SessionState::Answering,
            "answering moved off Answering on silence alone"
        );
        assert!(
            !answer.carrier_detected(),
            "carrier detected on silence alone"
        );

        // A real Originate-band idle-mark tone - exactly what a connected
        // originate's Tx transmits before any data is queued - must raise
        // carrier and move this end to Connected.
        let mut tx = Tx::new(cfg(Role::Originate));
        let mut tone = vec![0.0f32; 8000];
        tx.read(&mut tone);
        for chunk in tone.chunks(BLOCK) {
            answer.process_in(chunk);
        }
        assert!(
            answer.carrier_detected(),
            "no carrier after a real Originate-band tone"
        );
        assert_eq!(
            answer.state(),
            SessionState::Connected,
            "answering did not move to Connected once carrier arrived"
        );

        // And once the tone actually stops for long enough to clear the
        // hold-off, carrier_detected() must track that too, not latch.
        for _ in 0..20 {
            answer.process_in(&silence);
        }
        assert!(
            !answer.carrier_detected(),
            "carrier stayed detected long after the signal stopped"
        );
    }
}
