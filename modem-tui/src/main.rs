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
//! # Terminal restoration on every exit path
//!
//! `ratatui::run` (see its own doc) initialises the terminal, installs a
//! panic hook that restores it before the default panic handler prints
//! anything, runs the closure, and restores the terminal afterwards
//! regardless of what the closure returns - `Ok`, `Err`, or a panic
//! unwinding through it. That is the whole of this binary's own
//! responsibility for "restored on every exit path including panic":
//! rely on the already-proven mechanism rather than re-implementing it.

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

struct Args {
    layout: RequestedLayout,
    acoustic: bool,
}

fn parse_args() -> Result<Args, String> {
    let mut layout = RequestedLayout::default();
    let mut acoustic = false;
    let mut single_requested = false;
    for arg in std::env::args().skip(1) {
        match arg.as_str() {
            "--single" => {
                layout = RequestedLayout::Single;
                single_requested = true;
            }
            "--split" => layout = RequestedLayout::Split,
            "--acoustic" => acoustic = true,
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
    Ok(Args { layout, acoustic })
}

fn print_help() {
    println!("modem - a Bell 103 acoustic modem simulator\n");
    println!("Usage: modem [--single | --split] [--acoustic]\n");
    println!("  --split      both ends on one machine, for demonstration (default)");
    println!("  --single     one end, connected to another machine over a real device");
    println!("               (needs --acoustic)");
    println!("  --acoustic   use the real sound card instead of the wired demo transport");
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
            let pane = Pane::new(transport.config_for(Role::Originate, Duplex::HalfPingPong));
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
