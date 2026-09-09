//! One end of a call, with enough terminal-emulation state on top of
//! `modem_core::session::Session` and `modem_core::at::AtProcessor` to
//! drive the mockups: a scrollback, a composing line, and an elapsed
//! timer.
//!
//! # What "local echo off" does and does not mean here
//!
//! The design spec says chat rides "local echo off, so that what appears
//! genuinely came back over the air." That is a claim about the
//! *scrollback* - [`Pane::history`] - which only ever gains a line from
//! two sources: an AT command response (`AtProcessor` executing
//! something this end typed - a real Hayes modem echoes its own command
//! responses locally too, so this is not the thing the spec is warning
//! about) or bytes actually drained from [`Session::receive`], which by
//! construction only contains payload the packet layer decoded off the
//! wire.
//!
//! [`Pane::composing`] is a different thing: the line currently being
//! typed, kept only so the person typing it can see it, and never copied
//! into `history`. That is ordinary editing feedback, not a duplicate
//! "echo" of the transcript - the mockup shows exactly this: `hello from
//! the other side` (received, in history) sits above `> took you long
//! enough_` (still being composed, never yet in history).
//!
//! Bytes are still fed to the session one at a time as they are typed
//! (`AtProcessor::feed` per character, exactly as `at.rs` already
//! implements it) - `composing` is a display-only mirror of what has
//! already gone out, not a buffer withheld until Enter. That is true in
//! command mode always, and in data mode under `Duplex::Full`, where
//! there is no turn to manage and every keystroke can simply go out live.
//!
//! # Half-duplex chat: typing queues, Enter is what actually sends (Task 17)
//!
//! Under `Duplex::HalfPingPong`, data-mode typing does **not** stream
//! character by character - it accumulates in `composing` exactly like
//! command-mode editing does, and nothing reaches [`Session::send`] until
//! Enter closes the line. Two reasons, not one:
//!
//! - `Session::send` does not gate on `has_turn` (see its own doc - that
//!   is documented as the caller's responsibility), so streaming a
//!   keystroke the instant it is typed would queue it into `tx`
//!   regardless of whether this end currently holds the turn - exactly
//!   the caller mistake `session.rs`'s own tests guard against.
//! - Even when this end *does* hold the turn, a half-duplex link cannot
//!   assume the person typing will finish before some external event
//!   (the far end, a timeout, anything Task 18/19 might add) forces a
//!   hand-over - a whole line sent as one unit, then handed back, is the
//!   unit half-duplex chat actually deals in.
//!
//! Enter therefore closes the composed line, appends the wire's own
//! terminator (`\r`) and hands it to [`Pane::try_flush_pending`]: sent
//! immediately if this end already holds the turn, or left in
//! `pending_line` - "typing while the far end holds the turn queues
//! locally" - until [`Pane::tick`] notices `has_turn()` has become true.
//! Once the whole line, terminator included, has been handed to the
//! session, this end immediately [`Session::yield_turn`]s - `process_out`'s
//! own `yielding` bookkeeping (see `session.rs`) is what keeps the real
//! bytes and the turn token draining for real before this end goes back
//! to idle mark, so calling `yield_turn` here does not cut the message
//! off early. This is "the token is yielded when the line has drained",
//! achieved by relying on machinery `session.rs` already proves, not by
//! this module watching for drainage itself.
//!
//! # A settle gap after the turn arrives, before flushing
//!
//! `rx.rs`'s own module doc is explicit: a receiver needs an acquisition
//! preamble *and then at least one character time of idle mark* before
//! real data, or the opening of the burst can arrive mangled - and
//! `session.rs`'s own module doc names this as the reason every exchange
//! test in this crate pumps a short settle after acquiring the turn
//! before calling `send`. A line that was queued *before* the turn
//! arrived has no such gap by default: `grant_turn` queues the
//! acquisition preamble and this end's own `has_turn()` flips true in the
//! very same tick, so flushing immediately would put real data right on
//! the preamble's heels with no settle at all. [`Pane::try_flush_pending`]
//! therefore also requires `turn_held_for` - accumulated by [`Pane::tick`]
//! for as long as `has_turn()` reads true, reset the instant it does not -
//! to have reached [`TURN_SETTLE`] before it will actually send.

use std::mem;
use std::time::Duration;

use modem_core::at::AtProcessor;
use modem_core::session::{Session, SessionState};
use modem_core::{tones, Config, Duplex, Role};

/// How long this end must have continuously held the turn before
/// [`Pane::try_flush_pending`] will actually send a queued line - see
/// this module's own doc, "A settle gap after the turn arrives, before
/// flushing". Comfortably above `rx.rs`'s own documented floor (one
/// character time at 300 baud, about 33 ms) without being long enough
/// for a person actually typing to notice.
const TURN_SETTLE: Duration = Duration::from_millis(200);

/// One end of a call: the protocol state (`Session`, `AtProcessor`) plus
/// the terminal-emulation state a comms package needs on top of it.
pub struct Pane {
    /// The `Config` this pane was constructed with. Only ever used for
    /// `duplex` (which never changes) and as the template for
    /// `run_transport`'s throwaway placeholder session - **not** for
    /// `role`, which `dial`/`answer` can change after construction (Task
    /// 19). Read the live role from `session.role()` instead; see
    /// [`Pane::role_name`]'s own doc.
    cfg: Config,
    session: Session,
    at: AtProcessor,
    /// Completed (or still-open, see `receiving_open`) scrollback lines,
    /// oldest first. See this module's own doc for exactly what is and
    /// is not allowed to land here.
    history: Vec<String>,
    /// Whether `history.last()` is an in-progress line still being
    /// appended to by incoming bytes (no terminator seen yet) - real
    /// modem chat has no message framing, so a received line is only
    /// "complete" once a `\n` arrives, and until then this crate still
    /// wants to show characters landing as they come in rather than
    /// waiting.
    receiving_open: bool,
    /// The line currently being typed, for local display only - never
    /// copied into `history`. See this module's own doc.
    composing: String,
    /// A completed `Duplex::HalfPingPong` chat line, terminator included,
    /// waiting for this end to actually hold the turn - see this
    /// module's own doc on half-duplex chat. `None` under `Duplex::Full`,
    /// where nothing is ever queued this way, and while there is nothing
    /// left to send.
    pending_line: Option<Vec<u8>>,
    /// How long `session.has_turn()` has read continuously true, reset
    /// to zero the instant it does not - see [`TURN_SETTLE`] and this
    /// module's own doc.
    turn_held_for: Duration,
    /// Wall time accumulated while `session.state() == Connected`, for
    /// the status line's elapsed-time field. Not used by any test as a
    /// proxy for protocol correctness - see this project's own
    /// established caution against asserting on `SessionState`.
    elapsed: Duration,
    /// Set by [`Pane::set_auto_answer`] for a `--answer` end that must
    /// keep listening with nothing typed into it, matching a real
    /// unattended answering machine (`main.rs`'s own doc: "that end has
    /// to be listening from the moment it starts"). An unattended
    /// listener has to keep listening, not just start out listening: see
    /// [`Pane::tick`]'s own doc for the disclosed carrier-detection gap
    /// this actually guards against in practice.
    auto_answer: bool,
}

impl Pane {
    pub fn new(cfg: Config) -> Self {
        Pane {
            cfg,
            session: Session::new(cfg),
            at: AtProcessor::new(),
            history: Vec::new(),
            receiving_open: false,
            composing: String::new(),
            pending_line: None,
            turn_held_for: Duration::ZERO,
            elapsed: Duration::ZERO,
            auto_answer: false,
        }
    }

    /// Marks this pane as an unattended answering end: [`Pane::tick`]
    /// re-issues `session.answer()` on its own if the session ever falls
    /// back to `Idle` while this is set, rather than waiting for a human
    /// to notice a dropped call and retype `ATA`. `false` by default -
    /// only `--answer` mode's own startup auto-answer (see `main.rs`)
    /// should set this; a call a person actually typed `ATA` for, or hung
    /// up on purpose, must stay hung up.
    pub fn set_auto_answer(&mut self, on: bool) {
        self.auto_answer = on;
    }

    /// `ORIGINATE` or `ANSWER`, matching the mockups' own capitalisation.
    ///
    /// Reads [`Session::role`] (Task 19), not this pane's own `Config`
    /// snapshot: `dial`/`answer` can change which role the session
    /// actually is after this pane was constructed, and a title bar that
    /// kept reading the original `Config` would keep claiming `ORIGINATE`
    /// on an end that has since answered.
    pub fn role_name(&self) -> &'static str {
        match self.session.role() {
            Role::Originate => "ORIGINATE",
            Role::Answer => "ANSWER",
        }
    }

    /// The mark/space pair this end *transmits* in, e.g. `(1270.0,
    /// 1070.0)` - see `modem_core::tones`'s own doc for why this is the
    /// transmit band, not necessarily the band this end listens on. Reads
    /// [`Session::role`] for the same reason [`Pane::role_name`] does.
    pub fn band(&self) -> (f64, f64) {
        tones(self.session.role())
    }

    /// `"1270/1070"` - the band, formatted for the title bar. Whole
    /// hertz: Bell 103's tones are all round numbers, so a fractional
    /// part would only ever be display noise from the `f64`.
    pub fn band_label(&self) -> String {
        let (mark, space) = self.band();
        format!("{}/{}", mark as i64, space as i64)
    }

    pub fn duplex_label(&self) -> &'static str {
        match self.cfg.duplex {
            Duplex::Full => "full duplex",
            Duplex::HalfPingPong => "half duplex",
        }
    }

    /// What the status line's turn/state field should say. Pre-connect
    /// this names the call state in the mockups' own words ("listening"
    /// for an answering end still waiting on carrier); once connected
    /// under half duplex it names who currently holds the turn; under
    /// full duplex there is no turn to name, so it repeats the duplex
    /// mode instead - deliberately not blank, so the field never
    /// disappears out from under a fixed-position status line.
    pub fn turn_or_state_label(&self) -> String {
        match self.session.state() {
            SessionState::Idle => "idle".to_string(),
            SessionState::Dialling => "dialling".to_string(),
            SessionState::Answering => "listening".to_string(),
            SessionState::Connected => match self.cfg.duplex {
                Duplex::Full => "full duplex".to_string(),
                Duplex::HalfPingPong => {
                    if self.session.has_turn() {
                        "YOUR TURN".to_string()
                    } else {
                        "THEIR TURN".to_string()
                    }
                }
            },
        }
    }

    pub fn carrier(&self) -> bool {
        self.session.carrier_detected()
    }

    /// Whether this pane's own session currently holds permission to
    /// transmit real data under `Duplex::HalfPingPong` - see
    /// `Session::has_turn`'s own doc. An immutable read, unlike
    /// `session_mut().has_turn()`, so a caller checking this (a redraw, a
    /// status predicate) never needs a mutable borrow just to ask.
    pub fn has_turn(&self) -> bool {
        self.session.has_turn()
    }

    pub fn elapsed_label(&self) -> String {
        let secs = self.elapsed.as_secs();
        format!(
            "{:02}:{:02}:{:02}",
            secs / 3600,
            (secs / 60) % 60,
            secs % 60
        )
    }

    pub fn in_command_mode(&self) -> bool {
        self.at.in_command_mode()
    }

    pub fn history(&self) -> &[String] {
        &self.history
    }

    pub fn composing(&self) -> &str {
        &self.composing
    }

    /// Direct access for a caller (the CLI, eventually) that wants to
    /// drive `dial`/`answer`/`hangup` from function keys rather than a
    /// typed AT command - `F4`/`F10` in [`crate::app::App::handle_key`]
    /// use this rather than duplicating `Session`'s own API.
    pub fn session_mut(&mut self) -> &mut Session {
        &mut self.session
    }

    /// This pane's own `Config` - what a placeholder `Session`
    /// [`Pane::take_session`] swaps in should be built from.
    pub fn config(&self) -> Config {
        self.cfg
    }

    /// Swaps this pane's own `Session` out for `replacement`, returning
    /// whatever was really running. Exists for `App::run_transport`:
    /// `Transport::run` needs a genuinely contiguous `&mut [Session]`
    /// (see its own doc), and a pane owns its session privately, so this
    /// is how several panes' sessions get gathered into one for that one
    /// call and then handed back. Nothing observes a pane's session
    /// through any other method while it is swapped out - the whole
    /// exchange happens within one `run_transport` call.
    pub fn take_session(&mut self, replacement: Session) -> Session {
        mem::replace(&mut self.session, replacement)
    }

    /// The other half of [`Pane::take_session`]: hands a session back.
    pub fn restore_session(&mut self, session: Session) {
        self.session = session;
    }

    /// One character typed at this pane, mirrored into `composing` for
    /// local display (see this module's own doc for why that mirror is
    /// not "echo" in the sense the design spec warns against).
    ///
    /// `'\r'` and `'\n'` are both treated as Enter. In command mode this
    /// always executes the buffered line immediately (moving it, plus any
    /// response lines, into `history`), regardless of duplex - a real
    /// Hayes modem accepts AT commands whoever currently holds a
    /// half-duplex data link's turn, and command-mode bytes never reach
    /// `Session::send` at all (see `at.rs`'s own `feed_command_byte`).
    ///
    /// In data mode, behaviour depends on duplex - see this module's own
    /// doc:
    /// - `Duplex::Full` streams every keystroke to [`AtProcessor::feed`]
    ///   live, exactly as before this crate had a turn to manage; Enter
    ///   sends a literal `\r` and clears `composing`.
    /// - `Duplex::HalfPingPong` accumulates into `composing` only; Enter
    ///   closes the line, appends the wire terminator, and hands it to
    ///   [`Pane::try_flush_pending`].
    pub fn feed_char(&mut self, c: char) {
        let is_enter = c == '\r' || c == '\n';

        if self.at.in_command_mode() {
            if is_enter {
                let line = mem::take(&mut self.composing);
                self.push_line(line);
                if let Some(resp) = self.at.feed(b'\r', &mut self.session) {
                    for l in resp.lines {
                        self.push_line(l);
                    }
                }
            } else {
                self.composing.push(c);
                let mut buf = [0u8; 4];
                for &b in c.encode_utf8(&mut buf).as_bytes() {
                    let _ = self.at.feed(b, &mut self.session);
                }
            }
            return;
        }

        if self.cfg.duplex == Duplex::Full {
            if is_enter {
                // Data mode: '\r' is ordinary data, not a local command
                // to execute - it goes out over the wire like any other
                // byte.
                let _ = self.at.feed(b'\r', &mut self.session);
                self.composing.clear();
            } else {
                self.composing.push(c);
                let mut buf = [0u8; 4];
                for &b in c.encode_utf8(&mut buf).as_bytes() {
                    let _ = self.at.feed(b, &mut self.session);
                }
            }
            return;
        }

        // Duplex::HalfPingPong data mode: see this module's own doc.
        // Typing always queues locally; Enter is what actually tries to
        // send, immediately if this end already holds the turn or as
        // soon as it does otherwise.
        if is_enter {
            let line = mem::take(&mut self.composing);
            if !line.is_empty() {
                let mut bytes = line.into_bytes();
                bytes.push(b'\r');
                self.pending_line = Some(bytes);
                self.try_flush_pending();
            }
        } else {
            self.composing.push(c);
        }
    }

    /// Sends [`Pane::pending_line`], if there is one, this end currently
    /// holds the turn, *and* it has held it continuously for at least
    /// [`TURN_SETTLE`] - see this module's own doc, "A settle gap after
    /// the turn arrives, before flushing". Immediately yields the turn
    /// back afterwards: `Session::process_out`'s own `yielding`
    /// bookkeeping is what keeps the real bytes and the turn token
    /// draining for real before this end returns to idle mark, so calling
    /// `yield_turn` here does not cut the message off early - this is
    /// what "the token is yielded when the line has drained" actually
    /// reduces to.
    ///
    /// Called both right after Enter closes a line (in case this end
    /// already holds the turn, and has held it long enough) and from
    /// [`Pane::tick`] every tick (in case a pending line was queued while
    /// this end lacked the turn, and has since been granted it and
    /// settled) - a no-op either way once `pending_line` is `None`.
    fn try_flush_pending(&mut self) {
        if !self.session.has_turn() || self.turn_held_for < TURN_SETTLE {
            return;
        }
        let Some(bytes) = self.pending_line.take() else {
            return;
        };
        for b in bytes {
            let _ = self.at.feed(b, &mut self.session);
        }
        self.session.yield_turn();
    }

    /// Convenience for tests (and, later, the dialling directory - Task
    /// 18): types a whole line followed by Enter, one character at a
    /// time through [`Pane::feed_char`], exactly as a person typing it
    /// would.
    pub fn type_line(&mut self, s: &str) {
        for c in s.chars() {
            self.feed_char(c);
        }
        self.feed_char('\r');
    }

    /// Removes the last character of the line being composed. Editing
    /// only - a byte already fed to `AtProcessor::feed` has already gone
    /// out and cannot be recalled, matching how a real half-duplex link
    /// works.
    pub fn backspace(&mut self) {
        self.composing.pop();
    }

    /// Advances this pane's clock by `dt`: lets `AtProcessor::advance_time`
    /// report an unprompted `CONNECT 300`/`NO CARRIER`, drains anything
    /// the far end sent into `history`, accumulates the elapsed timer
    /// while connected, and - under `Duplex::HalfPingPong` - flushes a
    /// queued chat line the moment this end is granted the turn it was
    /// waiting for. See [`Pane::try_flush_pending`]'s own doc.
    ///
    /// Also re-arms an [`Pane::set_auto_answer`]-marked pane the instant it
    /// falls back to `Idle`. This is not only about a call that genuinely
    /// ended: `modem-core`'s carrier detector has a disclosed, pre-existing
    /// gap (see `modem-core/src/carrier.rs`'s own module doc on why a
    /// broadband transient can clear its rise threshold before the floor
    /// has calibrated) that a caller's own off-hook click can trip on an
    /// answering end's receiver well before any real signal exists - and
    /// Task 3i's ringback fix (a real, full 2.0 s inter-ring silence)
    /// gives that false-early "connected" state a genuine silent stretch
    /// long enough to then genuinely, correctly detect carrier loss and
    /// hang up, before the real call has actually arrived. An unattended
    /// answering machine has to survive that exactly as it would a real
    /// call ending: by going straight back to listening, not by staying
    /// dead until a person notices and retypes `ATA`.
    pub fn tick(&mut self, dt: Duration) {
        if let Some(resp) = self.at.advance_time(dt, &mut self.session) {
            for l in resp.lines {
                self.push_line(l);
            }
        }
        if self.auto_answer && self.session.state() == SessionState::Idle {
            self.session.answer();
        }
        let bytes = self.session.receive();
        if !bytes.is_empty() {
            self.push_received(&bytes);
        }
        if self.session.state() == SessionState::Connected {
            self.elapsed += dt;
        }
        if self.session.has_turn() {
            self.turn_held_for = self.turn_held_for.saturating_add(dt);
        } else {
            self.turn_held_for = Duration::ZERO;
        }
        self.try_flush_pending();
    }

    /// Pushes a complete line (an AT echo or response) - always closes
    /// any in-progress received line first, so a locally generated line
    /// never gets silently appended onto the tail of one the far end was
    /// still sending.
    fn push_line(&mut self, line: String) {
        self.receiving_open = false;
        self.history.push(line);
    }

    /// Appends received bytes to `history`, one character at a time,
    /// starting a fresh line after every `\n` and dropping `\r` - see
    /// this module's own doc for why a partial line is shown immediately
    /// rather than buffered until a terminator arrives.
    fn push_received(&mut self, bytes: &[u8]) {
        for &b in bytes {
            match b {
                b'\r' => {}
                b'\n' => self.receiving_open = false,
                _ => {
                    let ch = b as char;
                    if self.receiving_open {
                        if let Some(last) = self.history.last_mut() {
                            last.push(ch);
                        } else {
                            self.history.push(ch.to_string());
                            self.receiving_open = true;
                        }
                    } else {
                        self.history.push(ch.to_string());
                        self.receiving_open = true;
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use modem_core::Duplex;

    fn cfg(role: Role) -> Config {
        Config {
            sample_rate: 8000,
            role,
            duplex: Duplex::Full,
        }
    }

    #[test]
    fn typing_at_dt_shows_the_command_in_history_once_enter_is_pressed() {
        let mut pane = Pane::new(cfg(Role::Originate));
        pane.type_line("ATDT01234567890");
        assert_eq!(pane.history()[0], "ATDT01234567890");
        assert_eq!(pane.composing(), "");
    }

    /// Composing is visible while typing, and mid-line it is not yet in
    /// history - proves the "never copied into history until closed"
    /// claim the module doc makes, not just the end state.
    #[test]
    fn composing_is_visible_before_enter_and_not_yet_in_history() {
        let mut pane = Pane::new(cfg(Role::Originate));
        pane.feed_char('A');
        pane.feed_char('T');
        assert_eq!(pane.composing(), "AT");
        assert!(pane.history().is_empty());
    }

    #[test]
    fn at_command_gets_an_ok_response_line() {
        let mut pane = Pane::new(cfg(Role::Originate));
        pane.type_line("AT");
        assert_eq!(pane.history(), &["AT".to_string(), "OK".to_string()]);
    }

    #[test]
    fn received_bytes_land_in_history_and_composing_is_untouched() {
        let mut pane = Pane::new(cfg(Role::Originate));
        pane.feed_char('h');
        pane.feed_char('i');
        pane.push_received(b"hello\n");
        assert_eq!(pane.history(), &["hello".to_string()]);
        assert_eq!(
            pane.composing(),
            "hi",
            "a received line must never touch what is still being composed"
        );
    }

    #[test]
    fn backspace_removes_the_last_composed_character_only() {
        let mut pane = Pane::new(cfg(Role::Originate));
        pane.feed_char('A');
        pane.feed_char('T');
        pane.feed_char('X');
        pane.backspace();
        assert_eq!(pane.composing(), "AT");
    }

    #[test]
    fn role_and_band_labels_are_the_bell_103_pairs() {
        let originate = Pane::new(cfg(Role::Originate));
        assert_eq!(originate.role_name(), "ORIGINATE");
        assert_eq!(originate.band_label(), "1270/1070");
        let answer = Pane::new(cfg(Role::Answer));
        assert_eq!(answer.role_name(), "ANSWER");
        assert_eq!(answer.band_label(), "2225/2025");
    }

    /// Required test (Task 19): the title bar must follow the session's
    /// actual role once it dials or answers, never stay pinned to
    /// whatever `Config` the `Pane` happened to be constructed with. Both
    /// panes here are deliberately built the "wrong" way round -
    /// answering from an `Originate` config, dialling from an `Answer`
    /// one - so this can only pass if `role_name`/`band_label` genuinely
    /// read the session, not `self.cfg`; a version that still read
    /// `self.cfg.role` would report `ORIGINATE`/`1270-1070` for the pane
    /// that has actually answered.
    #[test]
    fn role_name_and_band_follow_the_session_once_it_dials_or_answers_not_the_panes_own_config() {
        let mut answered_from_originate_cfg = Pane::new(half_cfg(Role::Originate));
        answered_from_originate_cfg.session_mut().answer();
        assert_eq!(
            answered_from_originate_cfg.role_name(),
            "ANSWER",
            "the frame must not keep claiming ORIGINATE on an end that answered"
        );
        assert_eq!(answered_from_originate_cfg.band_label(), "2225/2025");

        let mut dialled_from_answer_cfg = Pane::new(half_cfg(Role::Answer));
        dialled_from_answer_cfg.session_mut().dial("1");
        assert_eq!(dialled_from_answer_cfg.role_name(), "ORIGINATE");
        assert_eq!(dialled_from_answer_cfg.band_label(), "1270/1070");
    }

    #[test]
    fn elapsed_label_is_zero_before_any_tick() {
        let pane = Pane::new(cfg(Role::Originate));
        assert_eq!(pane.elapsed_label(), "00:00:00");
    }

    // --- Task 17: the half-duplex chat layer -------------------------

    fn half_cfg(role: Role) -> Config {
        Config {
            sample_rate: 8000,
            role,
            duplex: Duplex::HalfPingPong,
        }
    }

    /// Cross-wires two panes' own sessions for one block and ticks both -
    /// the same shape `examples/mockup.rs`'s own `pump` uses, kept local
    /// to this test module since neither crate exposes it as part of its
    /// public API.
    const BLOCK: usize = 256;

    fn pump(a: &mut Pane, b: &mut Pane) {
        let dt = Duration::from_secs_f64(BLOCK as f64 / 8000.0);
        let mut from_a = [0.0f32; BLOCK];
        let mut from_b = [0.0f32; BLOCK];
        a.session_mut().process_out(&mut from_a);
        b.session_mut().process_out(&mut from_b);
        a.session_mut().process_in(&from_b);
        b.session_mut().process_in(&from_a);
        a.tick(dt);
        b.tick(dt);
    }

    /// `b` is re-armed with a fresh `ATA` if its session ever drops back
    /// to `Idle` before `a` has connected - see `modem-core/src/at.rs`'s
    /// `five_hand_overs_hold_the_call_up_and_connect_reports_exactly_
    /// once_each_end` for the same fix and the disclosed, pre-existing
    /// finding it exists for: the answering end's receiver can trip on
    /// originate's own off-hook transient well before dialling has even
    /// finished, and Task 3i's ringback fix (a real, full 2.0 s inter-ring
    /// silence, not a truncated 0.5 s tail) gives that false-early carrier
    /// a genuine silent gap long enough to actually drop and hang up
    /// during - which a fresh `ATA` recovers from exactly as a person
    /// re-answering a dropped call would.
    fn connect(a: &mut Pane, b: &mut Pane) {
        for _ in 0..4000 {
            pump(a, b);
            if b.session_mut().state() == SessionState::Idle
                && a.session_mut().state() != SessionState::Connected
            {
                b.type_line("ATA");
            }
            if a.session_mut().state() == SessionState::Connected
                && b.session_mut().state() == SessionState::Connected
            {
                return;
            }
        }
        panic!("sessions never both reached Connected");
    }

    fn settle(a: &mut Pane, b: &mut Pane) {
        for _ in 0..10 {
            pump(a, b);
        }
    }

    /// Required test: typing while the far end holds the turn queues
    /// entirely locally - `composing` still shows it, but nothing reaches
    /// the wire, no matter how long it waits, because `originate` never
    /// yields here. The companion test below is the other half: what
    /// happens once the turn actually does arrive.
    #[test]
    fn typing_without_the_turn_queues_locally_and_never_reaches_the_wire() {
        let mut originate = Pane::new(half_cfg(Role::Originate));
        let mut answer = Pane::new(half_cfg(Role::Answer));
        originate.type_line("ATDT1");
        answer.type_line("ATA");
        connect(&mut originate, &mut answer);
        settle(&mut originate, &mut answer);

        assert!(
            !answer.session_mut().has_turn(),
            "answer must not start with the turn"
        );
        for c in "hello".chars() {
            answer.feed_char(c);
        }
        assert_eq!(
            answer.composing(),
            "hello",
            "typing must still show locally while queued"
        );
        answer.feed_char('\r');
        assert_eq!(
            answer.composing(),
            "",
            "Enter must close the composing line even while it only queues"
        );

        for _ in 0..1000 {
            pump(&mut originate, &mut answer);
        }
        // Content, not emptiness - `originate.history()` already carries
        // its own "ATDT1"/"OK"/"CONNECT 300" lines from the handshake
        // above by this point, so the only claim that actually matters is
        // that the queued word itself never arrived.
        assert!(
            !originate.history().iter().any(|l| l.contains("hello")),
            "a queued line must never reach the wire while this end lacks the turn: {:?}",
            originate.history()
        );
    }

    /// The other half: once this end actually holds the turn - either
    /// immediately, or later because [`Pane::tick`] flushed a line that
    /// was waiting - Enter's queued line goes out for real, and the turn
    /// returns to the far end once it has drained, all driven through the
    /// real `feed_char`/`type_line` surface, never a raw `Session::send`.
    /// Proven on a real round trip: `answer`'s line queued before it ever
    /// held the turn arrives at `originate` only once `originate` has
    /// sent its own message and handed the turn over - nothing here polls
    /// `has_turn()` as its own proof.
    #[test]
    fn a_queued_line_sends_once_the_turn_arrives_and_the_turn_returns_afterwards() {
        let mut originate = Pane::new(half_cfg(Role::Originate));
        let mut answer = Pane::new(half_cfg(Role::Answer));
        originate.type_line("ATDT1");
        answer.type_line("ATA");
        connect(&mut originate, &mut answer);
        settle(&mut originate, &mut answer);

        // Answer queues a line long before it ever holds the turn.
        answer.type_line("hi there");
        for _ in 0..500 {
            pump(&mut originate, &mut answer);
        }
        assert!(
            !originate.history().iter().any(|l| l.contains("hi there")),
            "answer's queued line must not have reached originate yet: {:?}",
            originate.history()
        );

        // Originate, still holding the turn from `dial`, sends its own
        // line and (per `Pane::try_flush_pending`) yields the turn the
        // instant it has been handed to the session.
        originate.type_line("go ahead");
        for _ in 0..2000 {
            pump(&mut originate, &mut answer);
        }
        assert!(
            answer.history().iter().any(|l| l.contains("go ahead")),
            "answer never received originate's message: {:?}",
            answer.history()
        );

        // Answer now holds the turn; `tick` flushes its own queued line
        // automatically, with no further user action.
        for _ in 0..2000 {
            pump(&mut originate, &mut answer);
        }
        assert!(
            originate.history().iter().any(|l| l.contains("hi there")),
            "originate never received answer's queued message once answer got the turn: {:?}",
            originate.history()
        );
    }

    /// The chat layer's own acceptance test: a full message, sent through
    /// `Pane`'s real half-duplex queueing (typed via `feed_char`, not a
    /// raw `Session::send`), surviving Task 12's own simulated room -
    /// harmonic distortion (the 1070 Hz Originate space tone's second
    /// harmonic at 2140 Hz), a desk-distance reflection, 25 dB SNR
    /// ambient noise and a hard clip - at the exact parameters `modem-
    /// core`'s own `impair::tests::a_realistic_combined_room_scenario_
    /// stays_within_the_gate` names as "Task 17's own acceptance figure".
    /// Assert on the recovered bytes and their byte error rate against
    /// Task 8's own `MAX_BYTE_ERROR_RATE` gate - never on `SessionState`.
    ///
    /// The handshake itself runs clean: `impair.rs`'s own calibration
    /// harness applies its impairments to the data channel, not the
    /// overture (see its module doc), so this connects normally, then
    /// captures the real chat message off `originate`'s own `Session::
    /// process_out` for as many blocks as it genuinely takes to drain -
    /// the acquisition preamble, the message and the turn token together -
    /// impairs that one buffer, and feeds it into `answer`'s real
    /// receiver. Same shape as `impair.rs`'s own `air`/`demod` harness,
    /// sourced from a real `Pane` driving the real turn-taking logic this
    /// task adds, not a bare `Tx`.
    #[test]
    fn a_chat_message_survives_task_12s_simulated_room() {
        let mut originate = Pane::new(half_cfg(Role::Originate));
        let mut answer = Pane::new(half_cfg(Role::Answer));
        originate.type_line("ATDT1");
        answer.type_line("ATA");
        connect(&mut originate, &mut answer);
        settle(&mut originate, &mut answer);

        assert!(
            originate.session_mut().has_turn(),
            "originate must hold the turn to send"
        );
        let message = "hello from the other side";
        for c in message.chars() {
            originate.feed_char(c);
        }
        originate.feed_char('\r');

        // `AtProcessor::feed_data_byte` calls `Session::send` once per
        // byte, so this is *not* one packet for the whole line - it is
        // one small packet (sync, seq, kind, length, one payload byte,
        // two CRC bytes - about 7 bytes) per character, plus the turn
        // token `feed_char`'s own Enter handling queues right after the
        // line. 500 blocks at `BLOCK` samples each is this crate's own
        // established convention for "comfortably enough for one short
        // message under that overhead" - `session.rs` and `at.rs` both
        // budget exactly this many pump iterations per message exchange
        // - so this captures the same span directly, rather than trying
        // to hand-compute a tighter bit budget from the message length.
        const BLOCK: usize = 256;
        let mut air = Vec::new();
        for _ in 0..500 {
            let mut buf = [0.0f32; BLOCK];
            originate.session_mut().process_out(&mut buf);
            air.extend_from_slice(&buf);
        }

        // Task 12's own simulated room, at the exact parameters and
        // order `impair.rs`'s own combined-room test uses.
        modem_core::impair::harmonic_distortion(&mut air, 0.3);
        modem_core::impair::reverb(&mut air, 16, 0.3);
        modem_core::impair::add_awgn(&mut air, 25.0, 0x005E_ED17);
        modem_core::impair::clip(&mut air, 0.7);

        for chunk in air.chunks(BLOCK) {
            answer.session_mut().process_in(chunk);
            answer.tick(Duration::from_secs_f64(chunk.len() as f64 / 8000.0));
        }

        let received = answer.history().last().cloned().unwrap_or_default();
        let ber = modem_core::impair::measure_ber(message.as_bytes(), received.as_bytes());
        assert!(
            ber <= modem_core::impair::MAX_BYTE_ERROR_RATE,
            "chat message measured ber {ber} through the simulated room (gate {}), recovered {received:?}",
            modem_core::impair::MAX_BYTE_ERROR_RATE
        );
    }
}
