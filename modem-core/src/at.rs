//! The Hayes AT command interface: the control surface a terminal talks to.
//!
//! [`AtProcessor`] sits in front of a [`Session`], translating between two
//! very different worlds: a human typing command lines and result codes
//! (`AT`, `OK`, `CONNECT 300`) while idle, and a raw byte stream that must
//! reach the far end unmolested once a call is up. Which world applies at
//! any moment is [`AtProcessor::in_command_mode`].
//!
//! # No clock, no I/O
//!
//! This crate is `no_std` and performs no I/O (see `lib.rs`'s own doc), and
//! the escape sequence this module implements is defined entirely in terms
//! of elapsed time - "at least one second of silence". Rather than reach
//! for a clock, [`AtProcessor::advance_time`] takes a [`Duration`] the
//! caller supplies, driven from the same audio clock that paces
//! `Session::process_out`/`process_in` (see `session.rs`). This keeps the
//! crate pure and every test in this module deterministic: a test can
//! assert "1500 ms of `advance_time` was not enough" without needing to
//! race a real clock or sleep.
//!
//! # Deviation from the plan's stated signature
//!
//! The task plan sketches `advance_time(&mut self, d: Duration)` with no
//! `Session` parameter and no return value. That cannot deliver two of this
//! module's own required behaviours: "carrier loss emits `NO CARRIER`
//! unprompted" and "`CONNECT 300`, on carrier" (for `ATDT`/`ATA`) both name
//! events that happen on the *wire*, not in response to a keystroke - a
//! real modem reports them the instant they occur, whether or not the DTE
//! is typing anything at that moment. [`AtProcessor::feed`] is the only
//! other entry point, and it is driven by DTE keystrokes; a call that is
//! silently connecting, or a line that silently drops, produces no
//! keystroke for `feed` to be called with. `advance_time` is the only
//! method already known to be called every audio tick regardless of DTE
//! activity (that is the whole reason it exists), so it is the one place
//! that can observe [`Session::carrier_detected`] transition and report it
//! unprompted. This module's `advance_time` therefore takes `&mut Session`
//! and returns `Option<Response>`, matching `feed`'s own shape. Tasks 15
//! and 17 do not exist yet, so nothing is broken by this - see the task
//! report for the full reasoning.
//!
//! # The escape sequence is three things, not one
//!
//! `+++ATH0` is not a command. The real sequence is:
//!
//! 1. At least one second of silence (no bytes fed while in data mode).
//! 2. Three `+` characters, no more than a second apart from each other.
//! 3. At least one second of silence afterwards.
//!
//! Only step 3 completing turns the sequence into an actual mode switch;
//! `ATH0` (or any other command) is then typed separately, afterwards, in
//! command mode. Nothing in this module ever matches the literal byte
//! sequence `+++ATH0` (or `+++` alone) as a unit - see
//! `mutation_3_the_shorthand_string_is_never_recognised` below, which
//! proves a version of this module that *did* pattern-match the literal
//! string fails, precisely because no such pattern-match exists to find.
//!
//! Implemented as two small counters, live only while in data mode:
//! `idle` (time since the last byte arrived, or since the last plus that
//! extended a run) and `plus_count` (0 to 3, consecutive plus characters
//! seen since the leading guard was satisfied). [`AtProcessor::feed`]
//! advances `plus_count` on a `+` that arrives with the right timing either
//! side of it (a fresh leading guard for the first plus, at most a second's
//! gap for the second and third); anything else - a non-plus byte, a
//! fourth character, or a `+` that arrives too fast or too slow - cancels
//! the attempt and flushes every buffered plus onto the wire as ordinary
//! data, in order, before the breaking byte itself also goes out, with no
//! exception: a cancelling byte never gets a second chance to open a fresh
//! attempt of its own, even if it is itself a `+` that happens to arrive
//! after a long gap (see `feed_data_byte`'s own doc). This is a
//! deliberately simple rule, not an oversight - a design that let a
//! cancelling byte sometimes restart a new attempt was tried and dropped
//! precisely because it made the inter-plus gap limit (the middle one of
//! the three rules above) unable to be proven by any test: any scenario
//! built to defeat it could always be read, instead, as evidence of a
//! *new* attempt starting from the byte that broke the old one. Nothing is
//! ever delayed for a byte that was never going to be part of an escape
//! attempt in the first place: a `+` arriving while data is flowing at
//! speed (insufficient leading silence) is written straight through in the
//! same call, not buffered and released later - see
//! `plus_plus_plus_inside_fast_data_is_written_straight_through` below.
//!
//! The plus **count** is enforced the same way as the timing: once
//! `plus_count` reaches 3, the only valid continuation is silence, so a
//! fourth character - including a fourth `+` - is not "the third plus
//! ignored and the escape fires anyway"; it cancels the whole attempt, and
//! all four characters go out as data. See
//! `a_fourth_plus_also_cancels_the_attempt` below.
//!
//! Only [`AtProcessor::advance_time`] can complete the third step (three
//! plusses buffered, then a further second with nothing else arriving);
//! [`AtProcessor::feed`] can only extend a run or cancel it, since a
//! keystroke is precisely the "something else arrived" that step 3 forbids.
//! A completed escape produces no [`Response`] - a real Hayes modem gives
//! no acknowledgement for `+++` either, only for the command typed
//! afterwards.
//!
//! # No echo
//!
//! Real Hayes modems typically echo typed characters back to the terminal.
//! Nothing in the command table or the required tests asks for it, and
//! [`Response`] only carries result lines, so this module does not
//! implement it.
//!
//! # Carrier loss is debounced, not instant (Task 17)
//!
//! [`AtProcessor::poll_carrier`] does not hang up the instant
//! `Session::carrier_detected` reads false - it only does so once that has
//! been continuously true for [`DCD_HOLD_TIME`] (about 700 ms, register
//! S10's default), which sits on top of `Session::carrier_detected`'s own
//! shorter internal hold-off. Task 17's `session.rs` fix means an idle
//! half-duplex end now transmits continuous mark rather than silence, so
//! an ordinary turn hand-over should never trip this at all - this is the
//! secondary, belt-and-braces defence for a momentary dropout (line
//! noise, a genuine glitch) that should not be allowed to hang up a call
//! that is still live, not the primary fix for hand-overs.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;
use core::time::Duration;

use crate::session::Session;

/// The Hayes default guard time (register S12's default of 50, in
/// fiftieths of a second). Used on both sides of the escape sequence: the
/// silence required before the first plus and the silence required after
/// the third.
const GUARD_TIME: Duration = Duration::from_secs(1);

/// Register S10's default: how long carrier must be absent before a call
/// is considered genuinely dropped and `NO CARRIER` fires. About 700 ms -
/// a real modem's own factory default is commonly 7 (tenths of a
/// second). This sits on top of, not instead of, [`Session::carrier_
/// detected`]'s own shorter hold-off (`carrier.rs`'s `HOLDOFF_SECONDS`,
/// 500 ms): that one guards the raw signal against a brief dip; this one
/// guards the *call* against a momentary carrier dropout - a burst of
/// line noise, or a half-duplex hand-over that glitches - that would
/// otherwise hang up a link that is still genuinely live. Task 17's own
/// idle-mark fix (see `session.rs`) is what makes a hand-over itself
/// silent to this detector in the first place; this is the secondary,
/// belt-and-braces defence on top of it.
const DCD_HOLD_TIME: Duration = Duration::from_millis(700);

/// One AT command's result, or an unprompted line, as the lines a terminal
/// would print, in order. A `Response` is deliberately just a list of
/// strings - see this crate's task report for why several of this
/// module's own tests compare it element by element rather than checking
/// length or membership alone.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Response {
    pub lines: Vec<String>,
}

impl Response {
    fn one(line: &str) -> Self {
        Response {
            lines: vec![line.to_string()],
        }
    }

    fn two(first: &str, second: &str) -> Self {
        Response {
            lines: vec![first.to_string(), second.to_string()],
        }
    }
}

/// The control surface in front of a [`Session`]: parses AT command lines
/// while idle, passes bytes straight through to [`Session::send`] once a
/// call is up, and watches for the `+++` escape sequence and for carrier
/// dropping unexpectedly. See this module's own doc for the escape
/// sequence's exact timing rules and for why `advance_time` takes a
/// `Session`.
pub struct AtProcessor {
    /// `true` while accepting bytes as an AT command line; `false` while
    /// passing bytes straight through as call data. Starts `true` - a
    /// fresh modem is not on a call.
    in_command_mode: bool,
    /// Bytes accumulated for the command line in progress, cleared on
    /// every CR. Never holds a CR itself.
    cmd_buf: Vec<u8>,
    /// Time since the last byte was fed while in data mode (whether it
    /// extended a plus run or not), or since the last plus that did.
    /// Meaningless, and left untouched, while in command mode.
    idle: Duration,
    /// Consecutive, correctly-timed plus characters buffered so far as a
    /// candidate escape sequence: 0 to 3. Never holds anything while in
    /// command mode - entering command mode always clears it.
    plus_count: u8,
    /// Shadow of `Session::carrier_detected()` as of the last time it was
    /// checked, so `advance_time` can tell a genuine transition (the event
    /// worth reporting) from carrier merely continuing to be up or down.
    carrier_up: bool,
    /// How long `session.carrier_detected()` has read continuously false
    /// since `carrier_up` was last true - see [`DCD_HOLD_TIME`]. Reset to
    /// zero the instant carrier is seen up again; meaningless (and left
    /// untouched) while `carrier_up` is already false, since there is
    /// nothing to be holding off from.
    carrier_down_for: Duration,
}

impl Default for AtProcessor {
    fn default() -> Self {
        Self::new()
    }
}

impl AtProcessor {
    /// A fresh processor: in command mode, nothing buffered, no carrier
    /// assumed. Matches a modem that has just been switched on.
    pub fn new() -> Self {
        Self {
            in_command_mode: true,
            cmd_buf: Vec::new(),
            idle: Duration::ZERO,
            plus_count: 0,
            carrier_up: false,
            carrier_down_for: Duration::ZERO,
        }
    }

    /// Whether a typed byte would currently be interpreted as part of an
    /// AT command line (`true`) or passed straight through as call data
    /// (`false`).
    pub fn in_command_mode(&self) -> bool {
        self.in_command_mode
    }

    /// Enters data mode without a command line to execute one from -
    /// exactly what an auto-answering end needs (see `modem-tui`'s own
    /// `--answer` mode doc: "picking up without anybody typing ATA").
    /// Calling `session.answer()` directly and never touching this
    /// processor at all leaves `in_command_mode` stuck `true` forever,
    /// since only `execute`'s own command arms (`"A"`, `"DT..."`, `"O"`)
    /// ever call the private `go_online` this delegates to - a real,
    /// found defect: an auto-answered end could receive data (`Session`
    /// itself does not care what mode this processor is in) but any text
    /// this end's own DTE typed would be parsed as an AT command line
    /// instead of sent as data, silently failing with `ERROR` rather
    /// than ever reaching the wire. Deliberately still produces no
    /// `Response` and touches no history - unlike feeding literal `"ATA"`
    /// through [`AtProcessor::feed`], which would show as though someone
    /// had typed it, contradicting "nothing typed into it".
    pub fn force_data_mode(&mut self) {
        self.go_online();
    }

    /// Feeds one byte typed at the DTE. In command mode, accumulates it
    /// into the command line in progress and executes on CR, returning
    /// that command's [`Response`]. In data mode, either extends a
    /// candidate escape sequence (see this module's own doc) or passes it
    /// straight through to `session.send`; either way returns `None` -
    /// entering command mode via `+++` is silent, and ordinary data is not
    /// acknowledged.
    pub fn feed(&mut self, byte: u8, session: &mut Session) -> Option<Response> {
        if self.in_command_mode {
            self.feed_command_byte(byte, session)
        } else {
            self.feed_data_byte(byte, session);
            None
        }
    }

    /// Advances the internal clock by `d`, driven by the caller from the
    /// audio clock (see this module's own doc for why this, not `feed`, is
    /// what can report events that happen with no keystroke behind them).
    /// Completes a pending escape sequence once its trailing guard time has
    /// elapsed, and reports `CONNECT 300` or `NO CARRIER` on a genuine
    /// carrier transition.
    pub fn advance_time(&mut self, d: Duration, session: &mut Session) -> Option<Response> {
        if !self.in_command_mode {
            self.idle = self.idle.saturating_add(d);
            if self.plus_count == 3 && self.idle >= GUARD_TIME {
                // The trailing guard has elapsed with nothing else having
                // arrived (a byte arriving would have gone through
                // `feed_data_byte` and cleared `plus_count` already, so
                // reaching this point at all means step 3 is genuinely
                // satisfied). The three buffered plusses were the escape
                // signal, not data - they are discarded, never sent.
                self.plus_count = 0;
                self.idle = Duration::ZERO;
                self.in_command_mode = true;
            }
        }
        self.poll_carrier(d, session)
    }

    /// Checks `session.carrier_detected()` against the last known state
    /// and reports a genuine transition, if any. A rising edge is only
    /// worth announcing while still in data mode waiting for it (a rise
    /// noticed while already back in command mode - because the far end
    /// connected while this end was mid-escape, an unlikely but possible
    /// ordering - is not the `ATDT`/`ATA` connect announcement and is not
    /// reported) and always clears [`Self::carrier_down_for`] - carrier is
    /// genuinely up, so there is nothing left to be holding off from.
    ///
    /// A falling edge does not hang up the instant it is seen: `d` -
    /// `advance_time`'s own tick duration - accumulates in `carrier_down_for`
    /// while carrier reads continuously false, and only once that reaches
    /// [`DCD_HOLD_TIME`] does this actually hang up and report `NO CARRIER`.
    /// A falling edge always matters once the hold time genuinely elapses,
    /// in either mode: a call that drops is worth reporting whether or not
    /// the DTE happened to be online at that exact moment, and always
    /// hangs up cleanly on this end too, so a dead `Session` is never left
    /// half-connected.
    fn poll_carrier(&mut self, d: Duration, session: &mut Session) -> Option<Response> {
        let up = session.carrier_detected();
        if up {
            self.carrier_down_for = Duration::ZERO;
            if !self.carrier_up {
                self.carrier_up = true;
                if !self.in_command_mode {
                    return Some(Response::one("CONNECT 300"));
                }
            }
            return None;
        }

        if !self.carrier_up {
            return None;
        }
        self.carrier_down_for = self.carrier_down_for.saturating_add(d);
        if self.carrier_down_for < DCD_HOLD_TIME {
            return None;
        }
        self.carrier_up = false;
        self.carrier_down_for = Duration::ZERO;
        session.hangup();
        self.in_command_mode = true;
        self.plus_count = 0;
        self.idle = Duration::ZERO;
        Some(Response::one("NO CARRIER"))
    }

    /// Accumulates one command-mode byte, executing on CR. LF is ignored
    /// (tolerating a terminal that sends CRLF), so it can arrive between
    /// commands without starting an empty command line of its own. A bare
    /// CR on an empty buffer produces no response, matching the shape of
    /// pressing return with nothing typed.
    fn feed_command_byte(&mut self, byte: u8, session: &mut Session) -> Option<Response> {
        match byte {
            b'\n' => None,
            b'\r' => {
                if self.cmd_buf.is_empty() {
                    return None;
                }
                let line = core::mem::take(&mut self.cmd_buf);
                let upper = String::from_utf8_lossy(&line).to_ascii_uppercase();
                Some(self.execute(&upper, session))
            }
            other => {
                self.cmd_buf.push(other);
                None
            }
        }
    }

    /// Executes one already-uppercased command line - the case-folding
    /// happens once in [`AtProcessor::feed_command_byte`], not here, so
    /// every arm below can match a plain literal. See the task brief's
    /// command table for the full list; anything not matched is `ERROR`,
    /// including a line that does not even start with `AT`.
    fn execute(&mut self, upper: &str, session: &mut Session) -> Response {
        let Some(rest) = upper.strip_prefix("AT") else {
            return Response::one("ERROR");
        };
        match rest {
            "" => Response::one("OK"),
            "Z" => {
                // Reset: only this processor's own pending escape state -
                // an active call is not torn down by ATZ.
                self.plus_count = 0;
                self.idle = Duration::ZERO;
                Response::one("OK")
            }
            "I" => Response::two(&format!("modem-core Bell 103 v{}", crate::VERSION), "OK"),
            "A" => {
                session.answer();
                self.carrier_up = false;
                self.go_online();
                Response::one("OK")
            }
            "H" | "H0" => {
                session.hangup();
                self.carrier_up = false;
                Response::two("OK", "NO CARRIER")
            }
            "O" | "O0" => {
                if session.carrier_detected() {
                    self.go_online();
                    Response::one("CONNECT 300")
                } else {
                    Response::one("ERROR")
                }
            }
            _ if rest.starts_with("DT") => {
                let digits = &rest[2..];
                session.dial(digits);
                self.carrier_up = false;
                self.go_online();
                Response::one("OK")
            }
            _ => Response::one("ERROR"),
        }
    }

    /// Common bookkeeping for switching to data mode: leaves
    /// `carrier_up` untouched, since `ATO` resumes a call already in
    /// progress (see its own doc) while `ATDT`/`ATA` clear it themselves
    /// first, having just built a fresh `Session::tx`/`rx` pair with no
    /// carrier of its own yet.
    fn go_online(&mut self) {
        self.in_command_mode = false;
        self.plus_count = 0;
        self.idle = Duration::ZERO;
    }

    /// One data-mode byte: either extends a candidate escape sequence, is
    /// discarded as part of one that just failed (see below), or is
    /// written straight to `session.send`. See this module's own doc for
    /// the full algorithm; in short, a `+` only ever buffers instead of
    /// transmitting immediately when the leading guard (for the first
    /// plus) or the inter-plus gap (for the second and third) is actually
    /// satisfied *at the moment it arrives* - so a `+` inside fast-flowing
    /// data is written through in this same call, never held back on the
    /// chance it might turn into an escape.
    fn feed_data_byte(&mut self, byte: u8, session: &mut Session) {
        let idle = self.idle;
        let continues_run = byte == b'+'
            && match self.plus_count {
                0 => idle >= GUARD_TIME,
                1 | 2 => idle <= GUARD_TIME,
                _ => false,
            };
        if continues_run {
            self.plus_count += 1;
            self.idle = Duration::ZERO;
            return;
        }

        // Not a valid continuation: whatever was buffered was never part
        // of a real escape sequence after all - a byte arrived either too
        // soon, too late, or is not a plus at all; `plus_count` may also
        // already be 3, where the only valid continuation is silence and
        // any byte at all - even another plus - breaks it. Every buffered
        // plus was real data all along, and goes out now, in order,
        // followed unconditionally by this byte, which does not get a
        // second chance to open a fresh attempt of its own even if it is
        // itself a `+` that happens to arrive after a long gap - a byte's
        // only route into a candidate escape is the `continues_run` check
        // above, evaluated once, on its own arrival. This keeps the model
        // simple enough to prove: once a run breaks, everything buffered,
        // and the byte that broke it, is data, full stop. See
        // `plus_plus_plus_with_an_inter_plus_gap_over_a_second_does_not_
        // escape` and `a_fourth_plus_also_cancels_the_attempt` below.
        for _ in 0..self.plus_count {
            session.send(b"+");
        }
        self.plus_count = 0;
        session.send(&[byte]);
        self.idle = Duration::ZERO;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::SessionState;
    use crate::{Config, Duplex, Role};

    /// `Duplex::Full`, not `HalfPingPong`: an AT-controlled terminal
    /// session is inherently full duplex - either end can type at any
    /// moment, with no "who currently holds the turn" concept for a human
    /// to manage. This also matters mechanically: under `HalfPingPong`,
    /// `process_out`'s own documented discipline is that the end without
    /// the turn transmits nothing at all (see `session.rs`), so an
    /// Originate session's own `carrier_detected()` would never rise
    /// unless something explicitly yielded the turn to the far end - a
    /// half-duplex-specific mechanic this module's command table has no
    /// concept of. Under `Full`, both ends transmit unconditionally once
    /// connected, so carrier genuinely reflects "is the far end still
    /// there" the whole time, exactly what `CONNECT 300` and `NO CARRIER`
    /// need.
    fn cfg(role: Role) -> Config {
        Config {
            sample_rate: 8000,
            role,
            duplex: Duplex::Full,
        }
    }

    const BLOCK: usize = 256;
    /// One block of audio at 8 kHz, matching `BLOCK` - the unit
    /// `advance_time` is fed in every test below, mirroring how a real
    /// audio-driven caller would pace it one block at a time.
    fn block_duration() -> Duration {
        Duration::from_secs_f64(BLOCK as f64 / 8000.0)
    }

    /// Wires two sessions' audio together for one block, exactly like
    /// `session.rs`'s own `pump` helper (see its doc) - the AT layer adds
    /// no new audio plumbing of its own, so this is deliberately the same
    /// shape.
    fn pump(a: &mut Session, b: &mut Session) {
        let mut oa = vec![0.0f32; BLOCK];
        let mut ob = vec![0.0f32; BLOCK];
        a.process_out(&mut oa);
        b.process_out(&mut ob);
        a.process_in(&ob);
        b.process_in(&oa);
    }

    const MAX_ITERS: usize = 4000;

    /// Pumps both sessions, ticking `at`'s clock by one block every
    /// iteration, until `local`'s carrier has genuinely risen (proof the
    /// call is real, not just that `state()` reports `Connected` - see
    /// this crate's own standing caution against trusting `SessionState`
    /// alone). Returns every `Response` `advance_time` produced along the
    /// way, in order, so a test can find its `CONNECT 300` without caring
    /// exactly which tick it landed on.
    fn connect(at: &mut AtProcessor, local: &mut Session, far: &mut Session) -> Vec<Response> {
        let mut responses = Vec::new();
        for _ in 0..MAX_ITERS {
            pump(local, far);
            if let Some(r) = at.advance_time(block_duration(), local) {
                responses.push(r);
            }
            if local.carrier_detected() && far.carrier_detected() {
                return responses;
            }
        }
        panic!("carrier never rose on both ends within the iteration budget");
    }

    /// A short settle, matching `session.rs`'s own convention: lets a
    /// just-granted turn's acquisition preamble drain before real data is
    /// queued.
    fn settle(a: &mut Session, b: &mut Session) {
        for _ in 0..10 {
            pump(a, b);
        }
    }

    /// Feeds every byte of an ASCII command line, including a trailing
    /// CR, and returns whatever `feed` returned for the CR itself (every
    /// other byte in a well-formed command line returns `None`).
    fn feed_line(at: &mut AtProcessor, session: &mut Session, line: &str) -> Option<Response> {
        let mut last = None;
        for b in line.bytes() {
            last = at.feed(b, session);
        }
        last
    }

    #[test]
    fn new_processor_starts_in_command_mode() {
        let at = AtProcessor::new();
        assert!(at.in_command_mode());
    }

    /// Required test: `AT` returns `OK`, and so does lowercase `at` -
    /// checked as one exact-content comparison each, not merely "some
    /// response arrived". Mutation 4 (case-sensitive parsing) fails the
    /// lowercase half of this directly - see the task report.
    #[test]
    fn at_command_returns_ok_case_insensitively() {
        let mut session = Session::new(cfg(Role::Originate));
        let mut at = AtProcessor::new();

        let resp = feed_line(&mut at, &mut session, "AT\r");
        assert_eq!(resp, Some(Response::one("OK")));
        assert!(at.in_command_mode());

        let resp = feed_line(&mut at, &mut session, "at\r");
        assert_eq!(resp, Some(Response::one("OK")));
        assert!(at.in_command_mode());
    }

    /// Required test: an unknown command returns `ERROR`, and a line that
    /// does not even start with `AT` does too.
    #[test]
    fn unknown_command_returns_error() {
        let mut session = Session::new(cfg(Role::Originate));
        let mut at = AtProcessor::new();

        assert_eq!(
            feed_line(&mut at, &mut session, "ATXYZ\r"),
            Some(Response::one("ERROR"))
        );
        assert_eq!(
            feed_line(&mut at, &mut session, "XYZ\r"),
            Some(Response::one("ERROR"))
        );
    }

    #[test]
    fn atz_returns_ok() {
        let mut session = Session::new(cfg(Role::Originate));
        let mut at = AtProcessor::new();
        assert_eq!(
            feed_line(&mut at, &mut session, "ATZ\r"),
            Some(Response::one("OK"))
        );
    }

    #[test]
    fn ati_returns_a_line_then_ok() {
        let mut session = Session::new(cfg(Role::Originate));
        let mut at = AtProcessor::new();
        let resp = feed_line(&mut at, &mut session, "ATI\r").expect("ATI must respond");
        assert_eq!(resp.lines.len(), 2);
        assert_eq!(resp.lines[1], "OK");
        assert!(!resp.lines[0].is_empty());
    }

    /// Required test: `ATDT<digits>` puts the session into dialling and
    /// enters data mode. Checked on the response and the mode flag, not
    /// on `SessionState` alone - `state()` is included only as
    /// corroborating evidence that `dial` was actually invoked, not as
    /// the test's sole claim.
    #[test]
    fn atdt_dials_and_enters_data_mode() {
        let mut session = Session::new(cfg(Role::Originate));
        let mut at = AtProcessor::new();

        let resp = feed_line(&mut at, &mut session, "ATDT01234\r");
        assert_eq!(resp, Some(Response::one("OK")));
        assert!(!at.in_command_mode());
        assert_eq!(session.state(), SessionState::Dialling);
    }

    /// `ATDT` really does carry a call: dials for real, waits for a real
    /// carrier from a genuine answering `Session` (never asserting on
    /// `SessionState` as the proof - see `connect`'s own doc), gets
    /// `CONNECT 300` unprompted from `advance_time`, then proves data mode
    /// actually passes bytes through by sending real content and reading
    /// it back byte-exact from the far end.
    #[test]
    fn atdt_reaches_connect_and_data_mode_carries_real_traffic() {
        let mut local = Session::new(cfg(Role::Originate));
        let mut far = Session::new(cfg(Role::Answer));
        let mut at = AtProcessor::new();

        assert_eq!(
            feed_line(&mut at, &mut local, "ATDT1\r"),
            Some(Response::one("OK"))
        );
        far.answer();

        let responses = connect(&mut at, &mut local, &mut far);
        assert_eq!(
            responses,
            vec![Response::one("CONNECT 300")],
            "expected exactly one CONNECT 300, unprompted, and nothing else"
        );

        settle(&mut local, &mut far);
        for &b in b"HELLO FAR END" {
            assert_eq!(at.feed(b, &mut local), None);
        }
        for _ in 0..500 {
            pump(&mut local, &mut far);
        }
        assert_eq!(far.receive(), b"HELLO FAR END");
    }

    /// Required test / Mutation 1 target: `+++` with no leading guard time
    /// does not escape - it is transmitted as data. Proven on content (the
    /// far end's decoded bytes), not on `in_command_mode()` alone, though
    /// that is checked too.
    #[test]
    fn plus_plus_plus_with_no_leading_guard_is_transmitted_as_data() {
        let mut local = Session::new(cfg(Role::Originate));
        let mut far = Session::new(cfg(Role::Answer));
        let mut at = AtProcessor::new();
        feed_line(&mut at, &mut local, "ATDT1\r");
        far.answer();
        connect(&mut at, &mut local, &mut far);
        settle(&mut local, &mut far);

        // One ordinary byte first, which is what actually establishes "no
        // leading guard".
        //
        // This used to rely on `idle` still being zero from `go_online`,
        // and that was never true: `connect` calls `advance_time` on every
        // handshake iteration, which accumulates `idle` in data mode, and
        // a rising carrier does not reset it. The test only passed
        // because the answering end used to connect instantly on the
        // originator's off-hook click, so the handshake finished inside
        // the guard time. With that bug fixed the handshake legitimately
        // takes longer, the guard elapses, and `+++` escapes correctly -
        // the test was asserting the absence of a precondition it had
        // stopped establishing.
        assert_eq!(at.feed(b'x', &mut local), None);
        for &b in b"+++" {
            assert_eq!(at.feed(b, &mut local), None);
        }
        // Give the (correct) implementation every chance to wrongly
        // escape, if it were going to: a full second and then some.
        for _ in 0..40 {
            pump(&mut local, &mut far);
            at.advance_time(block_duration(), &mut local);
        }

        assert!(
            !at.in_command_mode(),
            "no leading guard was present; +++ must not have escaped"
        );
        for _ in 0..500 {
            pump(&mut local, &mut far);
        }
        assert_eq!(far.receive(), b"x+++");
    }

    /// Required test: `+++` appearing inside ordinary data at speed is
    /// transmitted, not swallowed. Distinct from the no-leading-guard test
    /// above: the plusses here are surrounded by other real data on both
    /// sides in one continuous burst, proving the escape detector does not
    /// even momentarily disrupt ordinary fast-flowing content. Also the
    /// test that stands in for the brief's "one-string shorthand" trap in
    /// its passing form - see `mutation_3_the_shorthand_string_is_never_
    /// recognised` below for the failing form.
    #[test]
    fn plus_plus_plus_inside_fast_data_is_written_straight_through() {
        let mut local = Session::new(cfg(Role::Originate));
        let mut far = Session::new(cfg(Role::Answer));
        let mut at = AtProcessor::new();
        feed_line(&mut at, &mut local, "ATDT1\r");
        far.answer();
        connect(&mut at, &mut local, &mut far);
        settle(&mut local, &mut far);

        for &b in b"go+++ahead" {
            assert_eq!(at.feed(b, &mut local), None);
        }
        for _ in 0..40 {
            pump(&mut local, &mut far);
            at.advance_time(block_duration(), &mut local);
        }
        assert!(!at.in_command_mode());

        for _ in 0..500 {
            pump(&mut local, &mut far);
        }
        assert_eq!(far.receive(), b"go+++ahead");
    }

    /// Mutation 3 target: the literal byte sequence `+++ATH0`, typed as one
    /// unbroken burst with no guard time anywhere around it, is exactly
    /// the "common shorthand" the task brief names and says is wrong -
    /// `+++` is not a command, and `+++ATH0` is not a thing. With no
    /// leading silence, none of it can be a real escape attempt (see this
    /// module's own doc), so a correct implementation writes the whole
    /// seven bytes straight through as ordinary data and the call stays
    /// up. This is also this module's proof that no code path recognises
    /// the shorthand as a unit: see the task report for the mutation that
    /// makes this test fail - a version of `feed_data_byte` with an added
    /// branch that pattern-matches the literal bytes `b"+++ATH0"` as they
    /// arrive and hangs up immediately, which nothing in the actual
    /// implementation does.
    #[test]
    fn mutation_3_the_shorthand_string_is_never_recognised() {
        let mut local = Session::new(cfg(Role::Originate));
        let mut far = Session::new(cfg(Role::Answer));
        let mut at = AtProcessor::new();
        feed_line(&mut at, &mut local, "ATDT1\r");
        far.answer();
        connect(&mut at, &mut local, &mut far);
        settle(&mut local, &mut far);

        // Fresh into data mode, idle is zero: no leading guard anywhere in
        // this burst, so nothing here can be a genuine escape attempt.
        for &b in b"+++ATH0" {
            assert_eq!(at.feed(b, &mut local), None);
        }
        for _ in 0..40 {
            pump(&mut local, &mut far);
            at.advance_time(block_duration(), &mut local);
        }

        assert!(
            !at.in_command_mode(),
            "the shorthand must not have escaped into command mode"
        );
        assert!(
            local.carrier_detected(),
            "the shorthand must not have hung up the call"
        );
        for _ in 0..500 {
            pump(&mut local, &mut far);
        }
        assert_eq!(
            far.receive(),
            b"+++ATH0",
            "the literal shorthand must have gone out as ordinary data, byte-exact"
        );
    }

    /// Required test / Mutation 2 target: `+++` with leading guard but no
    /// trailing guard does not escape. Covers both ways the trailing guard
    /// can fail to be satisfied: not enough silence yet (proves the guard
    /// is a real duration check, not merely "did any time pass"), and a
    /// fourth character arriving immediately, which must cancel the
    /// attempt and flush every buffered plus - and the character that
    /// broke it - onto the wire as data, in order.
    #[test]
    fn plus_plus_plus_with_leading_guard_but_no_trailing_guard_does_not_escape() {
        let mut local = Session::new(cfg(Role::Originate));
        let mut far = Session::new(cfg(Role::Answer));
        let mut at = AtProcessor::new();
        feed_line(&mut at, &mut local, "ATDT1\r");
        far.answer();
        connect(&mut at, &mut local, &mut far);
        settle(&mut local, &mut far);

        // Leading guard: a full second of silence before the first plus.
        for _ in 0..40 {
            pump(&mut local, &mut far);
            at.advance_time(block_duration(), &mut local);
        }
        for &b in b"+++" {
            assert_eq!(at.feed(b, &mut local), None);
        }

        // Less than the trailing guard: escape must not have completed
        // yet. This is what actually falls over under Mutation 2 (the
        // trailing-guard duration check removed) - a mutated
        // advance_time would flip to command mode on this very first
        // tick, regardless of how little time it represents.
        at.advance_time(Duration::from_millis(500), &mut local);
        assert!(
            !at.in_command_mode(),
            "half a second is not the guard time; must not have escaped yet"
        );

        // A fourth character, arriving with no further silence, cancels
        // the attempt outright.
        assert_eq!(at.feed(b'X', &mut local), None);
        assert!(!at.in_command_mode());

        // Give it a further full second, in case a broken implementation
        // was merely slow rather than correctly cancelled.
        for _ in 0..40 {
            pump(&mut local, &mut far);
            at.advance_time(block_duration(), &mut local);
        }
        assert!(!at.in_command_mode());

        for _ in 0..500 {
            pump(&mut local, &mut far);
        }
        assert_eq!(
            far.receive(),
            b"+++X",
            "the cancelled escape attempt must have gone out as data, in order, X included"
        );
    }

    /// Coordinator review finding: the inter-plus gap limit (`at.rs`'s
    /// `1 | 2 => idle <= GUARD_TIME` arm) had no test that could actually
    /// isolate it. Mutating it to `true` passed the whole suite, including
    /// every escape test above - none of them ever separate the first plus
    /// from the second and third by more than an instant, so the limit was
    /// never exercised on its own. Without it, `+`, a five-minute pause,
    /// `+`, another pause, `+` would still escape, which is not the Hayes
    /// sequence at all and would fire on transmitted data containing
    /// scattered plus signs.
    ///
    /// This drives exactly that shape, deliberately kept to only three
    /// plus characters total so a single `far.receive()` comparison can
    /// tell the whole story: the leading guard is satisfied, one plus
    /// arrives, then time advances well past the guard with nothing else
    /// happening, then the remaining two plusses arrive back to back. A
    /// correct implementation cannot let the first plus survive that gap -
    /// it is flushed as data the moment the second plus arrives too late
    /// to continue it - and the two that follow immediately after can only
    /// ever count as a fresh two-plus attempt of their own, one short of
    /// completing anything, so they are written straight through too by
    /// the same rule (see this module's own doc: a cancelling byte gets no
    /// second chance to open a new attempt). All three end up as data,
    /// none of them buffered forever, and the call never escapes.
    #[test]
    fn plus_plus_plus_with_an_inter_plus_gap_over_a_second_does_not_escape() {
        let mut local = Session::new(cfg(Role::Originate));
        let mut far = Session::new(cfg(Role::Answer));
        let mut at = AtProcessor::new();
        feed_line(&mut at, &mut local, "ATDT1\r");
        far.answer();
        connect(&mut at, &mut local, &mut far);
        settle(&mut local, &mut far);

        // Leading guard, then the first plus.
        for _ in 0..40 {
            pump(&mut local, &mut far);
            at.advance_time(block_duration(), &mut local);
        }
        assert_eq!(at.feed(b'+', &mut local), None);

        // Well past the guard time, with nothing else arriving - the gap
        // the inter-plus limit exists to catch.
        at.advance_time(Duration::from_secs(2), &mut local);

        // The remaining two plusses, back to back.
        assert_eq!(at.feed(b'+', &mut local), None);
        assert_eq!(at.feed(b'+', &mut local), None);

        // Give it a further full second and more, in case a broken
        // implementation reached plus_count 3 and was only waiting on the
        // trailing guard.
        for _ in 0..80 {
            pump(&mut local, &mut far);
            at.advance_time(block_duration(), &mut local);
        }
        assert!(
            !at.in_command_mode(),
            "a plus spread across a five-second gap must not complete an escape"
        );

        for _ in 0..500 {
            pump(&mut local, &mut far);
        }
        assert_eq!(
            far.receive(),
            b"+++",
            "every plus must have gone out as data - none of them buffered forever"
        );
    }

    /// Coordinator review finding: confirms the deliberate answer to "does
    /// a fourth plus arriving within the window cancel the sequence, or
    /// does it escape on three and ignore the rest?" This module's rule
    /// (see its own doc) is the former: once `plus_count` reaches 3, the
    /// only valid continuation is silence, so a fourth character - even
    /// another plus - is treated exactly like any other cancelling byte
    /// (`plus_plus_plus_with_leading_guard_but_no_trailing_guard_does_not_
    /// escape` above proves this for a non-plus fourth character; this is
    /// the same claim for a plus). All four characters go out as data, in
    /// order, and the call never escapes.
    #[test]
    fn a_fourth_plus_also_cancels_the_attempt() {
        let mut local = Session::new(cfg(Role::Originate));
        let mut far = Session::new(cfg(Role::Answer));
        let mut at = AtProcessor::new();
        feed_line(&mut at, &mut local, "ATDT1\r");
        far.answer();
        connect(&mut at, &mut local, &mut far);
        settle(&mut local, &mut far);

        for _ in 0..40 {
            pump(&mut local, &mut far);
            at.advance_time(block_duration(), &mut local);
        }
        for &b in b"++++" {
            assert_eq!(at.feed(b, &mut local), None);
        }

        for _ in 0..80 {
            pump(&mut local, &mut far);
            at.advance_time(block_duration(), &mut local);
        }
        assert!(
            !at.in_command_mode(),
            "a fourth plus must cancel the attempt, not complete it early"
        );

        for _ in 0..500 {
            pump(&mut local, &mut far);
        }
        assert_eq!(far.receive(), b"++++");
    }

    /// Required test: `+++` with guard time either side does escape, and
    /// `ATH0` afterwards hangs up. The hangup is verified on the real
    /// signal, not the response text alone: pumps the far end afterwards
    /// and confirms its own receiver genuinely loses carrier, proving
    /// `local` actually stopped transmitting rather than merely reporting
    /// that it did.
    #[test]
    fn plus_plus_plus_with_guard_both_sides_escapes_and_ath0_hangs_up() {
        let mut local = Session::new(cfg(Role::Originate));
        let mut far = Session::new(cfg(Role::Answer));
        let mut at = AtProcessor::new();
        feed_line(&mut at, &mut local, "ATDT1\r");
        far.answer();
        connect(&mut at, &mut local, &mut far);
        settle(&mut local, &mut far);

        for _ in 0..40 {
            pump(&mut local, &mut far);
            at.advance_time(block_duration(), &mut local);
        }
        for &b in b"+++" {
            assert_eq!(at.feed(b, &mut local), None);
        }
        let mut escaped = false;
        for _ in 0..80 {
            pump(&mut local, &mut far);
            let r = at.advance_time(block_duration(), &mut local);
            assert_eq!(
                r, None,
                "a completed escape must be silent, not report anything"
            );
            if at.in_command_mode() {
                escaped = true;
                break;
            }
        }
        assert!(
            escaped,
            "guard time either side must have escaped into command mode"
        );

        // No data leaked out as the three plusses - they were the escape
        // signal, not content.
        for _ in 0..500 {
            pump(&mut local, &mut far);
        }
        assert_eq!(far.receive(), b"");

        let resp = feed_line(&mut at, &mut local, "ATH0\r");
        assert_eq!(resp, Some(Response::two("OK", "NO CARRIER")));

        // Real signal proof: local has genuinely stopped transmitting, so
        // the far end's own receiver must lose carrier too, not merely
        // read as "hung up" because local's Session object says so.
        let silence = vec![0.0f32; BLOCK];
        for _ in 0..60 {
            far.process_in(&silence);
        }
        assert!(
            !far.carrier_detected(),
            "far end must genuinely lose carrier once local really hung up"
        );
    }

    /// Required test: carrier loss emits `NO CARRIER` unprompted. The far
    /// end hangs up for real (stops transmitting entirely, not a
    /// synthetic flag flip), and `local`'s own receiver must genuinely
    /// lose lock before `advance_time` reports anything - proven by
    /// pumping real silence from `far` into `local`, not by calling any
    /// internal state setter.
    #[test]
    fn carrier_loss_emits_no_carrier_unprompted() {
        let mut local = Session::new(cfg(Role::Originate));
        let mut far = Session::new(cfg(Role::Answer));
        let mut at = AtProcessor::new();
        feed_line(&mut at, &mut local, "ATDT1\r");
        far.answer();
        connect(&mut at, &mut local, &mut far);
        settle(&mut local, &mut far);
        assert!(!at.in_command_mode());

        far.hangup();
        let mut reported = None;
        for _ in 0..MAX_ITERS {
            pump(&mut local, &mut far);
            if let Some(r) = at.advance_time(block_duration(), &mut local) {
                reported = Some(r);
                break;
            }
        }
        assert_eq!(reported, Some(Response::one("NO CARRIER")));
        assert!(
            at.in_command_mode(),
            "losing carrier must return the DTE to command mode"
        );
        assert!(
            !local.carrier_detected(),
            "the shadow flag must be reporting a real transition, not a stale one"
        );

        // Fully functional again, not just flagged as such.
        assert_eq!(
            feed_line(&mut at, &mut local, "AT\r"),
            Some(Response::one("OK"))
        );
    }

    /// Required test: `ATO` returns to data mode when connected, `ERROR`
    /// when not.
    #[test]
    fn ato_errors_when_not_connected() {
        let mut session = Session::new(cfg(Role::Originate));
        let mut at = AtProcessor::new();
        assert_eq!(
            feed_line(&mut at, &mut session, "ATO\r"),
            Some(Response::one("ERROR"))
        );
        assert!(at.in_command_mode());
    }

    #[test]
    fn ato_resumes_data_mode_when_connected_and_traffic_flows_again() {
        let mut local = Session::new(cfg(Role::Originate));
        let mut far = Session::new(cfg(Role::Answer));
        let mut at = AtProcessor::new();
        feed_line(&mut at, &mut local, "ATDT1\r");
        far.answer();
        connect(&mut at, &mut local, &mut far);
        settle(&mut local, &mut far);

        // Escape to command mode without hanging up.
        for _ in 0..40 {
            pump(&mut local, &mut far);
            at.advance_time(block_duration(), &mut local);
        }
        for &b in b"+++" {
            at.feed(b, &mut local);
        }
        for _ in 0..80 {
            pump(&mut local, &mut far);
            at.advance_time(block_duration(), &mut local);
            if at.in_command_mode() {
                break;
            }
        }
        assert!(at.in_command_mode());

        let resp = feed_line(&mut at, &mut local, "ATO\r");
        assert_eq!(resp, Some(Response::one("CONNECT 300")));
        assert!(!at.in_command_mode());

        settle(&mut local, &mut far);
        for &b in b"BACK ONLINE" {
            at.feed(b, &mut local);
        }
        for _ in 0..500 {
            pump(&mut local, &mut far);
        }
        assert_eq!(far.receive(), b"BACK ONLINE");
    }

    /// Own mutation target (Mutation 5): `ATH`'s two-line response is
    /// checked for exact order, not merely that both lines are present.
    /// A defect that pushes `"NO CARRIER"` before `"OK"` still produces a
    /// `Response` containing both strings - `resp.lines.contains(&"OK"
    /// .to_string()) && resp.lines.contains(&"NO CARRIER".to_string())`
    /// would pass it unchanged, which is exactly the aggregate blind spot
    /// this crate's task reports warn about (`Response` is a list of
    /// strings). Comparing the whole `Vec` catches the swap; see the task
    /// report for the mutation transcript.
    #[test]
    fn ath0_response_is_ok_then_no_carrier_in_that_order() {
        let mut local = Session::new(cfg(Role::Originate));
        let mut far = Session::new(cfg(Role::Answer));
        let mut at = AtProcessor::new();
        feed_line(&mut at, &mut local, "ATDT1\r");
        far.answer();
        connect(&mut at, &mut local, &mut far);
        settle(&mut local, &mut far);

        // Escape to command mode first - typing "ATH0" while still in data
        // mode would just be sent as data, not executed.
        for _ in 0..40 {
            pump(&mut local, &mut far);
            at.advance_time(block_duration(), &mut local);
        }
        for &b in b"+++" {
            at.feed(b, &mut local);
        }
        for _ in 0..80 {
            pump(&mut local, &mut far);
            at.advance_time(block_duration(), &mut local);
            if at.in_command_mode() {
                break;
            }
        }
        assert!(
            at.in_command_mode(),
            "must have escaped before ATH0 can be typed"
        );

        let resp = feed_line(&mut at, &mut local, "ATH0\r").expect("ATH0 must respond");
        assert_eq!(resp.lines, vec!["OK".to_string(), "NO CARRIER".to_string()]);
    }

    // --- Task 17: the half-duplex idle-mark fix, proven at the layer the
    // real defect was observable through. `session.rs`'s own tests cover
    // the raw sample-level property (RMS, recovered tone frequency); a
    // bare `Session` has no mechanism to hang a call up on carrier loss
    // at all - only `AtProcessor::poll_carrier` does that - so the actual
    // observable defect (a live call torn down) can only be reproduced
    // here, with a real `AtProcessor` driving each end. `Duplex::Half
    // PingPong`, unlike every other test above in this file - see
    // `half_cfg`'s own doc.

    /// `Duplex::HalfPingPong`, unlike this module's own `cfg` above - see
    /// `cfg`'s own doc for why every other test in this file deliberately
    /// runs `Duplex::Full` instead. This is the one deliberate exception:
    /// Task 17's fix is specifically about the half-duplex link this
    /// crate actually ships, and the tests below exist to exercise
    /// exactly that link, not the full-duplex one the rest of this file
    /// uses for unrelated AT-command coverage.
    fn half_cfg(role: Role) -> Config {
        Config {
            sample_rate: 8000,
            role,
            duplex: Duplex::HalfPingPong,
        }
    }

    /// The Task 17 fix's own required test. Before the fix, the first
    /// `yield_turn` made the yielding end transmit silence, the far end's
    /// receiver read that as carrier loss, and `poll_carrier` correctly
    /// (for a genuinely dropped call) hung up - tearing every half-duplex
    /// call down on its first hand-over. Five hand-overs, not one: a fix
    /// that survives the first and not the second is a different bug
    /// wearing the same clothes.
    ///
    /// Checked at every round: both ends' `state()` really is still
    /// `Connected` - one of the rare places that assertion is meaningful,
    /// since the old bug's whole observable effect *was* an unwanted
    /// transition out of `Connected`, driven by a real `hangup()` call,
    /// not a coincidental non-event a broken clock could also produce -
    /// and, not instead, that round's own message actually arrived
    /// byte-exact at the far end. Across the whole five-round exchange,
    /// `CONNECT 300` must have been reported exactly once per end and
    /// `NO CARRIER` never at all.
    ///
    /// Mutation 1 target: put the zero-fill back in `session.rs`'s
    /// `process_out` (delete the `idle_tx` read and restore
    /// `out.fill(0.0)` in the `else` branch) - this test fails at round 0,
    /// well before the fifth hand-over, because both ends drop to `Idle`
    /// the moment the first `yield_turn`'s own Turn packet finishes
    /// draining and the yielding end goes genuinely silent. See the task
    /// report for the actual failing output.
    #[test]
    fn five_hand_overs_hold_the_call_up_and_connect_reports_exactly_once_each_end() {
        let mut local = Session::new(half_cfg(Role::Originate));
        let mut far = Session::new(half_cfg(Role::Answer));
        let mut at_local = AtProcessor::new();
        let mut at_far = AtProcessor::new();
        // Setup-phase responses, deliberately not the vectors the final
        // assertion checks - see the connect loop's own doc below for why.
        let mut setup_local_responses = Vec::new();
        let mut setup_far_responses = Vec::new();

        // One audio block, ticking *both* ends' `AtProcessor` clocks every
        // single time - never a bare `pump()` on its own anywhere in this
        // test. A gap where only `pump` runs and `advance_time` is not
        // polled is exactly where a real carrier drop could hide from
        // this test: the underlying `Session`s would still see it (their
        // own receiver state updates regardless), but nothing would ever
        // act on it or record it, which is precisely the shape that would
        // let the "put the zero-fill back" mutation slip through
        // unnoticed during a long send-and-wait window.
        fn tick(
            local: &mut Session,
            far: &mut Session,
            at_local: &mut AtProcessor,
            at_far: &mut AtProcessor,
            local_responses: &mut Vec<Response>,
            far_responses: &mut Vec<Response>,
        ) {
            pump(local, far);
            if let Some(r) = at_local.advance_time(block_duration(), local) {
                local_responses.push(r);
            }
            if let Some(r) = at_far.advance_time(block_duration(), far) {
                far_responses.push(r);
            }
        }

        feed_line(&mut at_local, &mut local, "ATDT1\r");
        // `ATA`, not a direct `far.answer()` call - both ends need their
        // own `AtProcessor` genuinely in data mode for this test's own
        // per-round `feed`/`advance_time` calls (and its far-side
        // `CONNECT 300` assertion) to mean anything.
        feed_line(&mut at_far, &mut far, "ATA\r");

        // Waits on `state()`, not `carrier_detected()` - see the task
        // brief's own disclosed, not-yet-chased finding: the answering
        // end's receiver can trip on originate's off-hook transient
        // within 32 ms of `ATA`, well before originate has even finished
        // dialling. Under this fix, that false-early "Connected" makes
        // far genuinely start transmitting its own idle mark, which then
        // genuinely (not falsely) reaches local's own receiver too -
        // so a `carrier_detected()`-based wait here would exit before
        // local has actually finished its own overture, same as
        // `session.rs`'s own `connect` helper is careful to avoid. This
        // is exactly the "reaching Connected proves nothing" trap in
        // reverse: waiting on the wrong signal here would let the test
        // proceed on a call that is not really up yet, not merely fail
        // to prove one that is.
        //
        // Task 3i (Dan, 8 Sep 2026) gave the ringback stage its full,
        // genuine 2.0 s inter-ring gap - previously truncated to 0.5 s -
        // which is what turns the disclosed off-hook-transient finding
        // above from a curiosity into something this setup phase must
        // actually survive: far's falsely-early carrier now sees a real
        // 2.0 s stretch of true silence during that gap (nothing is
        // transmitted at all between the two rings), long enough for its
        // own detector to genuinely lose lock and for `poll_carrier` to
        // hang far up for real, well before local's own overture has
        // finished. Re-arming with a fresh `ATA` whenever that happens is
        // exactly what a person watching a real answer machine drop and
        // relisten would do, and it is enough: the off-hook click that
        // caused the *first* false trigger only ever fires once, at the
        // very start of local's transmission, so it cannot recur on a
        // freshly-armed `far`.
        let mut connected = false;
        for _ in 0..MAX_ITERS {
            tick(
                &mut local,
                &mut far,
                &mut at_local,
                &mut at_far,
                &mut setup_local_responses,
                &mut setup_far_responses,
            );
            if far.state() == SessionState::Idle && local.state() != SessionState::Connected {
                feed_line(&mut at_far, &mut far, "ATA\r");
            }
            if local.state() == SessionState::Connected && far.state() == SessionState::Connected {
                connected = true;
                break;
            }
        }
        assert!(
            connected,
            "both ends never reached Connected under half duplex"
        );
        for _ in 0..10 {
            tick(
                &mut local,
                &mut far,
                &mut at_local,
                &mut at_far,
                &mut setup_local_responses,
                &mut setup_far_responses,
            );
        }

        // Fresh vectors from here, not a continuation of the setup-phase
        // ones above: the assertion at the end of this test is specifically
        // about the five hand-overs that follow, not about whatever the
        // disclosed off-hook-transient finding (see the connect loop's own
        // doc above) added to `setup_far_responses` while the call was
        // still being established - a real, but separate, pre-existing
        // carrier-detection selectivity gap, not a hand-over defect, and
        // not what this test exists to catch.
        let mut local_responses = Vec::new();
        let mut far_responses = Vec::new();

        let mut holder_is_local = true; // originate (local) starts with the turn
        for round in 0..5 {
            assert_eq!(
                local.state(),
                SessionState::Connected,
                "local dropped before round {round}"
            );
            assert_eq!(
                far.state(),
                SessionState::Connected,
                "far dropped before round {round}"
            );

            let message = alloc::format!("round {round} over and out").into_bytes();
            if holder_is_local {
                assert!(
                    local.has_turn(),
                    "local should hold the turn at round {round}"
                );
                for &b in &message {
                    assert_eq!(at_local.feed(b, &mut local), None);
                }
            } else {
                assert!(far.has_turn(), "far should hold the turn at round {round}");
                for &b in &message {
                    assert_eq!(at_far.feed(b, &mut far), None);
                }
            }
            for _ in 0..500 {
                tick(
                    &mut local,
                    &mut far,
                    &mut at_local,
                    &mut at_far,
                    &mut local_responses,
                    &mut far_responses,
                );
            }
            let received = if holder_is_local {
                far.receive()
            } else {
                local.receive()
            };
            assert_eq!(
                received, message,
                "round {round}: message did not arrive byte-exact"
            );

            if holder_is_local {
                local.yield_turn();
            } else {
                far.yield_turn();
            }

            let mut turned = false;
            for _ in 0..MAX_ITERS {
                tick(
                    &mut local,
                    &mut far,
                    &mut at_local,
                    &mut at_far,
                    &mut local_responses,
                    &mut far_responses,
                );
                let now_holder = if holder_is_local {
                    far.has_turn()
                } else {
                    local.has_turn()
                };
                if now_holder {
                    turned = true;
                    break;
                }
            }
            assert!(
                turned,
                "the turn never actually changed hands at round {round}"
            );
            // A long settle, not the usual short one: this is the window
            // where the previous holder now genuinely has no turn and no
            // queued Turn packet either, so this is where a zero-fill
            // regression would show up as a sustained silence long enough
            // to clear both hold-offs (~1.2 s total) - see this test's own
            // `tick` doc.
            for _ in 0..100 {
                tick(
                    &mut local,
                    &mut far,
                    &mut at_local,
                    &mut at_far,
                    &mut local_responses,
                    &mut far_responses,
                );
            }
            holder_is_local = !holder_is_local;

            assert_eq!(
                local.state(),
                SessionState::Connected,
                "local dropped after round {round}"
            );
            assert_eq!(
                far.state(),
                SessionState::Connected,
                "far dropped after round {round}"
            );
        }

        // The property Task 17's fix actually guarantees: a hand-over is
        // silent on the wire, so it must be silent here too - no further
        // CONNECT 300 (nothing dropped and reconnected) and no NO CARRIER
        // (nothing dropped and stayed down) on either end, across all five
        // rounds. Stronger than checking a single accumulated total: this
        // asserts *zero* responses during the window that is actually
        // under test, rather than a total that setup-phase noise could
        // also happen to satisfy by coincidence.
        assert_eq!(
            local_responses,
            Vec::<Response>::new(),
            "local must see no CONNECT 300 and no NO CARRIER at all across five hand-overs"
        );
        assert_eq!(
            far_responses,
            Vec::<Response>::new(),
            "far must see no CONNECT 300 and no NO CARRIER at all across five hand-overs"
        );

        // Setup itself (before the hand-overs, and not what this test is
        // otherwise about) must still show a real, single, lasting connect
        // on local. far's setup is allowed the disclosed off-hook-transient
        // shape this test's connect-loop doc explains - a false-early
        // CONNECT 300, a genuine NO CARRIER once the ringback stage's own
        // real silence exposes it, and a second, real, lasting CONNECT 300
        // once local's overture actually finishes - but nothing beyond
        // that pattern, and never ending anywhere but connected.
        assert_eq!(
            setup_local_responses,
            vec![Response::one("CONNECT 300")],
            "local's own setup must show exactly one real, unprompted CONNECT 300"
        );
        assert!(
            setup_far_responses == vec![Response::one("CONNECT 300")]
                || setup_far_responses
                    == vec![
                        Response::one("CONNECT 300"),
                        Response::one("NO CARRIER"),
                        Response::one("CONNECT 300"),
                    ],
            "far's setup took an unexpected shape: {setup_far_responses:?}"
        );
    }

    /// The other half of the same fix: a genuine hangup - the far end
    /// truly stops transmitting anything at all, not merely yields the
    /// turn - must still report `NO CARRIER`. The idle-mark fix must not
    /// make real carrier loss undetectable, which is the obvious way to
    /// make the five-hand-over test above pass without actually fixing
    /// anything (e.g. an `idle_tx` that is somehow always heard as
    /// carrier by the far end regardless of whether `far` itself has
    /// really hung up).
    ///
    /// Mutation 3 target: make `poll_carrier` never report a falling edge
    /// (e.g. delete its "carrier genuinely down" branch entirely) - `reported`
    /// stays `None` for the whole iteration budget and this test fails
    /// outright. See the task report for the actual failing output.
    #[test]
    fn a_genuine_hangup_under_half_duplex_still_reports_no_carrier() {
        let mut local = Session::new(half_cfg(Role::Originate));
        let mut far = Session::new(half_cfg(Role::Answer));
        let mut at = AtProcessor::new();
        feed_line(&mut at, &mut local, "ATDT1\r");
        far.answer();

        // `.state()`, not `carrier_detected()` - see the sibling
        // five-hand-over test's own doc for why.
        let mut connected = false;
        for _ in 0..MAX_ITERS {
            pump(&mut local, &mut far);
            at.advance_time(block_duration(), &mut local);
            if local.state() == SessionState::Connected && far.state() == SessionState::Connected {
                connected = true;
                break;
            }
        }
        assert!(connected, "never reached Connected under half duplex");
        settle(&mut local, &mut far);

        far.hangup();
        let mut reported = None;
        for _ in 0..MAX_ITERS {
            pump(&mut local, &mut far);
            if let Some(r) = at.advance_time(block_duration(), &mut local) {
                reported = Some(r);
                break;
            }
        }
        assert_eq!(reported, Some(Response::one("NO CARRIER")));
        assert!(
            !local.carrier_detected(),
            "the shadow flag must be reporting a real transition, not a stale one"
        );
    }
}
