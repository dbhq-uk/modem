//! `modem`, the binary: the event loop that ties [`App`], a
//! [`modem_audio::Transport`] and a real terminal together.
//!
//! # The event loop
//!
//! Poll crossterm for a key with a short timeout, advance the transport,
//! tick the app, redraw - once per iteration, never blocking longer than
//! [`TICK`]. `Transport::run` (see its own module doc) is built exactly
//! so this is safe: it advances one call's worth of work and returns
//! immediately, never sleeping, so it can share one thread with reading
//! a keypress and redrawing without either starving the other.
//!
//! # Wired demo vs `--acoustic`, and why they are driven differently
//!
//! `--acoustic` drives everything through [`App::run_transport`], which
//! calls the real [`modem_audio::Transport::run`] - essential for
//! [`modem_audio::CpalTransport`], whose whole point is hiding a real
//! sound card's ring-buffer pacing behind that one call.
//!
//! The wired demo (the default) instead uses [`App::step_wired`], which
//! cross-wires the two panes' sessions directly rather than going
//! through [`modem_audio::WiredTransport::run`] - see that method's own
//! doc for why: `Transport::run`'s return value (`RunStats`) reports
//! block counts, never the actual samples exchanged, and the wired
//! demo's real audio is exactly what feeds the waterfall. A
//! `WiredTransport` is still built and used for its metadata
//! (`is_acoustic`, `sample_rate`, `config_for`) - only its own
//! `run`/`step` is bypassed.
//!
//! # `--single` needs `--acoustic`
//!
//! The wired demo always cross-wires exactly two sessions - that is the
//! entire mechanism, see `modem-audio`'s own doc: "the two ends never
//! touch the real playback device's samples: they are cross-wired
//! directly in software". A single visible pane with no real second end
//! to wire it to has nothing to demonstrate, so `--single` without
//! `--acoustic` is rejected at startup with an explanation, rather than
//! silently promoted to a layout nobody asked for or left to panic
//! later inside `App::step_wired`.
//!
//! # `--answer` picks receive mode, and that end answers by itself
//!
//! Bell 103 needs the two ends in opposite bands - originate transmits on
//! 1270/1070 and listens on 2225/2025, and the answering end does the
//! reverse. That split is what lets both talk at once over one pair, and
//! it is also why two machines started identically could not hear each
//! other at all, which is the bug Task 19 fixed.
//!
//! The role follows the command: `Session::dial` sets `Role::Originate`
//! and `Session::answer` sets `Role::Answer`, each rebuilding that end's
//! transmitter and receiver in the right band, exactly as `ATD` and `ATA`
//! did on a real modem.
//!
//! `--answer` is how you choose receive mode on the second machine, and
//! an end told to receive **answers at startup** rather than waiting to
//! be told twice. A real modem did the same thing through its S0
//! register: set it, and the modem picked up on its own with nobody
//! typing `ATA`. So the two-machine flow is `modem --single --acoustic`
//! on the calling machine and the same plus `--answer` on the receiving
//! one, then `ATDT<digits>` on the caller. Nothing to type on the far
//! end at all.

use std::time::{Duration, Instant};

use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};

use modem_audio::{CpalTransport, Transport, WiredTransport};
use modem_core::{Duplex, Role};
use modem_tui::{App, Pane, RequestedLayout, Theme};

/// Poll timeout for one event-loop tick. Comfortably under one 300-baud
/// character time (about 33 ms) and one ordinary video frame, so the
/// waterfall keeps scrolling and a reply keeps appearing smoothly between
/// keystrokes without spinning the CPU.
const TICK: Duration = Duration::from_millis(20);

#[derive(Debug, PartialEq)]
struct Args {
    layout: RequestedLayout,
    acoustic: bool,
    /// Which mode a `--single` end runs in. `Role::Answer` does not just
    /// choose a band: that end **auto-answers at startup**, so it is
    /// already listening before anybody types anything. Ignored for
    /// `--split`, which always builds one end of each role regardless.
    role: Role,
}

/// Reads real process arguments. A thin wrapper over
/// [`parse_args_from`] so the actual parsing and validation logic can be
/// unit-tested against a literal argument list, never the real, global
/// `std::env::args()`.
fn parse_args() -> Result<Args, String> {
    parse_args_from(std::env::args().skip(1))
}

fn parse_args_from(args: impl IntoIterator<Item = String>) -> Result<Args, String> {
    let mut layout = RequestedLayout::default();
    let mut acoustic = false;
    let mut single_requested = false;
    let mut role = Role::Originate;
    let mut answer_requested = false;
    for arg in args {
        match arg.as_str() {
            "--single" => {
                layout = RequestedLayout::Single;
                single_requested = true;
            }
            "--split" => layout = RequestedLayout::Split,
            "--acoustic" => acoustic = true,
            "--answer" => {
                role = Role::Answer;
                answer_requested = true;
            }
            "-h" | "--help" => {
                print_help();
                std::process::exit(0);
            }
            other => return Err(format!("unrecognised argument {other:?} - try --help")),
        }
    }
    if single_requested && !acoustic {
        return Err(
            "--single without --acoustic has nothing to demo - the wired demo always \
             cross-wires two ends. Pass --acoustic for a real one-ended call, or drop \
             --single for the two-pane demo."
                .to_string(),
        );
    }
    if answer_requested && layout != RequestedLayout::Single {
        return Err(
            "--answer only means something for --single - it puts one end in receive mode. \
             Pass --single --acoustic --answer, or drop --answer for the two-pane demo, \
             which already starts one end in each band."
                .to_string(),
        );
    }
    Ok(Args {
        layout,
        acoustic,
        role,
    })
}

fn print_help() {
    println!("modem - a Bell 103 acoustic modem simulator\n");
    println!("Usage: modem [--single | --split] [--acoustic] [--answer]\n");
    println!("  --split      both ends on one machine, for demonstration (default)");
    println!("  --single     one end, connected to another machine over a real device");
    println!("               (needs --acoustic)");
    println!("  --acoustic   use the real sound card instead of the wired demo transport");
    println!("  --answer     run this --single end in receive mode: it answers at");
    println!("               startup and waits in the answer band, so there is nothing");
    println!("               to type on it. Two machines: --single --acoustic on the");
    println!("               calling one, the same plus --answer on the receiving one,");
    println!("               then ATDT<digits> on the caller.");
    println!();
    println!("Dial with ATDT<digits>, answer with ATA - typed into either pane, followed");
    println!("by Enter. F2 opens the dialling directory (Up/Down to pick, Enter to dial,");
    println!("Esc to close), F4 answers, F7 swaps focus, F10 hangs up. Ctrl+C quits.");
    println!();
    println!("The dialling directory reads $MODEM_DIRECTORY, or else");
    println!("$XDG_CONFIG_HOME/modem/directory.tsv, or else ~/.config/modem/directory.tsv.");
    println!("Tab-separated name/number/note, one per line, note optional, '#' at the very");
    println!("start of a line for a comment. This binary only ever reads that file.");
}

fn main() {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("modem: {e}");
            std::process::exit(1);
        }
    };

    let result = ratatui::run(|terminal| run(terminal, args));
    if let Err(e) = result {
        eprintln!("modem: {e}");
        std::process::exit(1);
    }
}

fn run(
    terminal: &mut ratatui::DefaultTerminal,
    args: Args,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut transport: Box<dyn Transport> = if args.acoustic {
        Box::new(CpalTransport::new(true)?)
    } else {
        Box::new(WiredTransport::new(8000))
    };

    let mut app = match args.layout {
        RequestedLayout::Single => {
            let mut pane = Pane::new(transport.config_for(args.role, Duplex::HalfPingPong));
            if args.role == Role::Answer {
                // Auto-answer. `--answer` is how you pick receive mode on
                // the second machine, so that end has to be listening
                // from the moment it starts - a real modem did exactly
                // this through its S0 register, picking up without
                // anybody typing ATA. Choosing the mode and then still
                // having to type the command would be picking it twice.
                pane.session_mut().answer();
                // And keeps listening, not just starts out listening -
                // see `Pane::set_auto_answer`'s own doc for why an
                // unattended end has to survive a dropped call (real or
                // the disclosed early-carrier false start) the same way
                // a real answering machine goes straight back to
                // listening rather than staying dead.
                pane.set_auto_answer(true);
            }
            App::single(pane, &*transport, Theme::default())
        }
        RequestedLayout::Split => {
            let a = Pane::new(transport.config_for(Role::Originate, Duplex::HalfPingPong));
            let b = Pane::new(transport.config_for(Role::Answer, Duplex::HalfPingPong));
            App::split(a, b, &*transport, Theme::default())
        }
    };

    let mut last_tick = Instant::now();
    loop {
        let timeout = TICK.saturating_sub(last_tick.elapsed());
        if event::poll(timeout)? {
            if let Event::Key(key) = event::read()? {
                let is_quit = key.kind == KeyEventKind::Press
                    && key.code == KeyCode::Char('c')
                    && key.modifiers.contains(KeyModifiers::CONTROL);
                if is_quit {
                    return Ok(());
                }
                app.handle_key(key);
            }
        }

        if args.acoustic {
            app.run_transport(&mut *transport)?;
        } else {
            app.step_wired();
        }

        let dt = last_tick.elapsed();
        last_tick = Instant::now();
        app.tick(dt);

        terminal.draw(|f| f.render_widget(&app, f.area()))?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    /// No flags at all: split, wired demo, and a role that only matters
    /// once `--single` is also given.
    #[test]
    fn no_flags_defaults_to_split_wired_originate() {
        let parsed = parse_args_from(args(&[])).expect("no flags is valid");
        assert_eq!(
            parsed,
            Args {
                layout: RequestedLayout::Split,
                acoustic: false,
                role: Role::Originate,
            }
        );
    }

    /// `--single --acoustic` alone, with no `--answer`, must still default
    /// to `Role::Originate` - this is the exact scenario the brief names
    /// as broken: two machines running this with no way to choose a role
    /// both need to be able to dial or answer, and originate is the
    /// sensible default for "nothing decided yet".
    #[test]
    fn single_acoustic_without_answer_defaults_to_originate() {
        let parsed =
            parse_args_from(args(&["--single", "--acoustic"])).expect("single+acoustic is valid");
        assert_eq!(parsed.role, Role::Originate);
    }

    /// The new flag this task adds: `--answer` picks the answer band for
    /// a single idle end, before anyone has typed `ATDT` or `ATA`.
    #[test]
    fn single_acoustic_answer_selects_the_answer_band() {
        let parsed = parse_args_from(args(&["--single", "--acoustic", "--answer"]))
            .expect("single+acoustic+answer is valid");
        assert_eq!(parsed.role, Role::Answer);
        assert_eq!(parsed.layout, RequestedLayout::Single);
        assert!(parsed.acoustic);
    }

    #[test]
    fn single_without_acoustic_is_rejected() {
        let err = parse_args_from(args(&["--single"])).unwrap_err();
        assert!(
            err.contains("--acoustic"),
            "error should explain --single needs --acoustic: {err}"
        );
    }

    /// `--answer` names which band a *single idle end* starts in - it has
    /// nothing to say without `--single`, so it is rejected the same way
    /// `--single` without `--acoustic` is, rather than silently doing
    /// nothing.
    #[test]
    fn answer_without_single_is_rejected() {
        let err = parse_args_from(args(&["--answer"])).unwrap_err();
        assert!(
            err.contains("--single"),
            "error should explain --answer only means something with --single: {err}"
        );
    }

    #[test]
    fn answer_with_split_is_rejected() {
        let err = parse_args_from(args(&["--split", "--answer"])).unwrap_err();
        assert!(
            err.contains("--single"),
            "error should explain --answer only means something with --single: {err}"
        );
    }

    #[test]
    fn unrecognised_argument_is_rejected() {
        let err = parse_args_from(args(&["--bogus"])).unwrap_err();
        assert!(err.contains("--bogus"));
    }
}

#[cfg(test)]
mod auto_answer_tests {
    use modem_core::{Config, Duplex, Role};
    use modem_tui::Pane;
    use std::time::Duration;

    /// Builds a pane exactly the way `run` does for `--single`, including
    /// the auto-answer. Kept in step with the real construction by being
    /// the same two lines; if `run` grows more setup this has to follow,
    /// and the end-to-end test below is what would notice.
    fn single_pane(role: Role) -> Pane {
        let mut pane = Pane::new(Config {
            sample_rate: 8000,
            role,
            duplex: Duplex::HalfPingPong,
        });
        if role == Role::Answer {
            pane.session_mut().answer();
            pane.set_auto_answer(true);
        }
        pane
    }

    /// The two-machine flow, end to end, with **nothing typed on the
    /// receiving end**: one machine runs `--single --acoustic --answer`
    /// and just sits there, the other dials, and a line of chat has to
    /// arrive. That is what "you pick receive mode" has to mean, and it
    /// is the claim the page makes.
    ///
    /// Asserted on the recovered bytes, never on `SessionState` - two
    /// ends can both reach `Connected` while being completely deaf to
    /// each other, which is exactly the bug Task 19 fixed.
    #[test]
    fn an_answer_mode_end_takes_a_call_with_nothing_typed_into_it() {
        const BLOCK: usize = 256;
        let dt = Duration::from_secs_f64(BLOCK as f64 / 8000.0);

        let mut caller = single_pane(Role::Originate);
        let mut receiver = single_pane(Role::Answer);

        // Only the calling machine is touched by a person.
        caller.type_line("ATDT5551234");

        let mut from_caller = [0.0f32; BLOCK];
        let mut from_receiver = [0.0f32; BLOCK];
        let mut sent = false;
        for _ in 0..4000 {
            caller.session_mut().process_out(&mut from_caller);
            receiver.session_mut().process_out(&mut from_receiver);
            caller.session_mut().process_in(&from_receiver);
            receiver.session_mut().process_in(&from_caller);
            caller.tick(dt);
            receiver.tick(dt);

            if !sent && caller.session_mut().has_turn() && caller.carrier() {
                caller.session_mut().send(b"knock knock\n");
                sent = true;
            }
            if receiver.history().iter().any(|l| l.contains("knock knock")) {
                return;
            }
        }
        panic!(
            "an --answer end never took the call with nothing typed into it. \
             caller history {:?}, receiver history {:?}",
            caller.history(),
            receiver.history()
        );
    }
}
