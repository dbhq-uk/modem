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
//! already gone out, not a buffer withheld until Enter.

use std::mem;
use std::time::Duration;

use modem_core::at::AtProcessor;
use modem_core::session::{Session, SessionState};
use modem_core::{tones, Config, Duplex, Role};

/// One end of a call: the protocol state (`Session`, `AtProcessor`) plus
/// the terminal-emulation state a comms package needs on top of it.
pub struct Pane {
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
    /// Wall time accumulated while `session.state() == Connected`, for
    /// the status line's elapsed-time field. Not used by any test as a
    /// proxy for protocol correctness - see this project's own
    /// established caution against asserting on `SessionState`.
    elapsed: Duration,
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
            elapsed: Duration::ZERO,
        }
    }

    /// `ORIGINATE` or `ANSWER`, matching the mockups' own capitalisation.
    pub fn role_name(&self) -> &'static str {
        match self.cfg.role {
            Role::Originate => "ORIGINATE",
            Role::Answer => "ANSWER",
        }
    }

    /// The mark/space pair this end *transmits* in, e.g. `(1270.0,
    /// 1070.0)` - see `modem_core::tones`'s own doc for why this is the
    /// transmit band, not necessarily the band this end listens on.
    pub fn band(&self) -> (f64, f64) {
        tones(self.cfg.role)
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

    /// One byte typed at this pane, run through the AT processor exactly
    /// as a real DTE keystroke would be, and mirrored into `composing`
    /// for local display (see this module's own doc for why that mirror
    /// is not "echo" in the sense the design spec warns against).
    ///
    /// `'\r'` and `'\n'` are both treated as Enter: in command mode this
    /// executes the buffered line (moving it, plus any response lines,
    /// into `history`); in data mode it sends a literal carriage return
    /// over the wire (an ordinary data byte to `AtProcessor::feed`, which
    /// does not special-case it) and clears `composing` to start the next
    /// line.
    pub fn feed_char(&mut self, c: char) {
        let is_enter = c == '\r' || c == '\n';
        let was_command_mode = self.at.in_command_mode();

        if was_command_mode && is_enter {
            let line = mem::take(&mut self.composing);
            self.push_line(line);
            if let Some(resp) = self.at.feed(b'\r', &mut self.session) {
                for l in resp.lines {
                    self.push_line(l);
                }
            }
            return;
        }

        if is_enter {
            // Data mode: '\r' is ordinary data, not a local command to
            // execute - it goes out over the wire like any other byte.
            let _ = self.at.feed(b'\r', &mut self.session);
            self.composing.clear();
            return;
        }

        self.composing.push(c);
        let mut buf = [0u8; 4];
        for &b in c.encode_utf8(&mut buf).as_bytes() {
            let _ = self.at.feed(b, &mut self.session);
        }
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
    /// the far end sent into `history`, and accumulates the elapsed timer
    /// while connected.
    pub fn tick(&mut self, dt: Duration) {
        if let Some(resp) = self.at.advance_time(dt, &mut self.session) {
            for l in resp.lines {
                self.push_line(l);
            }
        }
        let bytes = self.session.receive();
        if !bytes.is_empty() {
            self.push_received(&bytes);
        }
        if self.session.state() == SessionState::Connected {
            self.elapsed += dt;
        }
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

    #[test]
    fn elapsed_label_is_zero_before_any_tick() {
        let pane = Pane::new(cfg(Role::Originate));
        assert_eq!(pane.elapsed_label(), "00:00:00");
    }
}
