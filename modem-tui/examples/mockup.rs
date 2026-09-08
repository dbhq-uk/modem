//! Renders the real TUI to a standalone HTML page, so the layouts can be
//! reviewed in a browser without two terminals and two machines.
//!
//! Every frame on that page comes out of `App::render_into` - the same
//! code path the binary draws through - with the two ends genuinely
//! connected to each other over hand-wired audio (`process_out` on one
//! end fed straight into `process_in` on the other). Nothing on the page
//! is typed by hand, which is the point: a drawn mockup can promise a
//! layout the code does not produce.
//!
//! ```text
//! cargo run -p modem-tui --example mockup > /tmp/modem-tui.html
//! ```

use std::fmt::Write as _;
use std::time::Duration;

use modem_audio::WiredTransport;
use modem_core::{Config, Duplex, Role};
use modem_tui::{App, Pane, Theme};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Color;

/// One cross-feed step: both ends render their own outgoing block, then
/// each is handed the other's. Rendering first and feeding second is
/// what makes this a wire rather than an echo - feeding `a` from `b`
/// before `b` has produced this block's samples would hand it the
/// previous one and quietly halve the round trip.
///
/// Also appends the combined signal - both ends' outgoing blocks summed,
/// the actual mix a demo call plays to the speakers - onto `audio`, so
/// the waterfall this example renders is built from real samples the
/// call genuinely produced (dial tone, DTMF, ANSam, then both Bell 103
/// carriers), not a synthetic stand-in.
fn pump(a: &mut Pane, b: &mut Pane, blocks: usize, audio: &mut Vec<f32>) {
    const BLOCK: usize = 256;
    let dt = Duration::from_secs_f64(BLOCK as f64 / 8000.0);
    let mut from_a = [0.0f32; BLOCK];
    let mut from_b = [0.0f32; BLOCK];
    for _ in 0..blocks {
        a.session_mut().process_out(&mut from_a);
        b.session_mut().process_out(&mut from_b);
        for i in 0..BLOCK {
            audio.push(from_a[i] + from_b[i]);
        }
        a.session_mut().process_in(&from_b);
        b.session_mut().process_in(&from_a);
        a.tick(dt);
        b.tick(dt);
    }
}

/// Pumps until `done` holds, up to `cap` blocks. The overture runs for
/// something over ten seconds and its exact length is not this example's
/// business to hardcode, so this waits on the condition and gives up
/// loudly rather than pumping a guessed number of blocks and rendering
/// whatever state that happened to leave behind.
fn pump_until(
    a: &mut Pane,
    b: &mut Pane,
    cap: usize,
    what: &str,
    audio: &mut Vec<f32>,
    done: impl Fn(&Pane, &Pane) -> bool,
) {
    for _ in 0..cap {
        if done(a, b) {
            return;
        }
        pump(a, b, 1, audio);
    }
    panic!("gave up waiting for {what} after {cap} blocks");
}

fn pane(role: Role) -> Pane {
    Pane::new(Config {
        sample_rate: 8000,
        role,
        // Full duplex, deliberately: under `Duplex::HalfPingPong` an end
        // without the turn transmits silence, the far end reads that as
        // carrier loss and hangs the call up on the first hand-over. See
        // this file's own note in the repo docs - the frames below would
        // otherwise all show a dead call.
        duplex: Duplex::Full,
    })
}

/// Two ends mid-call: dialled, answered, connected, one line received and
/// the reply half typed. The state every frame on the page is rendered
/// from, and the audio every frame's waterfall is computed from - real
/// samples from a real (wired) call, not a synthetic stand-in.
fn call() -> (Pane, Pane, Vec<f32>) {
    let mut a = pane(Role::Originate);
    let mut b = pane(Role::Answer);
    let mut audio = Vec::new();

    a.type_line("ATDT01234567890");
    b.type_line("ATA");
    pump_until(
        &mut a,
        &mut b,
        3000,
        "both ends to connect",
        &mut audio,
        |a, b| a.carrier() && b.carrier() && a.history().iter().any(|l| l.contains("CONNECT")),
    );

    b.session_mut().send(b"hello from the other side\n");
    pump_until(
        &mut a,
        &mut b,
        3000,
        "the far end's line to arrive",
        &mut audio,
        |a, _| a.history().iter().any(|l| l.contains("other side")),
    );

    // A few seconds on the clock, so the elapsed field reads as a call in
    // progress rather than one that just started.
    pump(&mut a, &mut b, 800, &mut audio);

    for c in "took you long enough".chars() {
        a.feed_char(c);
    }
    pump(&mut a, &mut b, 20, &mut audio);

    (a, b, audio)
}

fn css(colour: Color) -> String {
    match colour {
        Color::Rgb(r, g, b) => format!("#{r:02X}{g:02X}{b:02X}"),
        // The crate styles everything it draws with an explicit theme
        // colour, so anything else is an unstyled cell - the ground.
        _ => "#0A0A0A".to_string(),
    }
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// One rendered buffer as `<pre>` markup, one `<span>` per run of cells
/// sharing a (foreground, background) pair rather than one per cell - a
/// 100x24 frame is 2400 cells and a span each makes the page four times
/// the size for no visible difference.
///
/// Both colours matter, not just the foreground: the waterfall's
/// half-block glyph carries its lower sub-band in the background colour
/// (see `waterfall.rs`'s own doc), and the below-threshold four-tone axis
/// paints its cells as a plain space on a coloured background with no
/// foreground at all. A version keyed on `cell.fg` alone (Task 15's own,
/// before there was any real waterfall data to lose) would render every
/// four-tone cell and every lower sub-band as if it were empty.
fn frame_html(buf: &Buffer) -> String {
    let mut out = String::new();
    for y in 0..buf.area.height {
        let mut run = String::new();
        let mut run_colours: Option<(Color, Color)> = None;
        for x in 0..buf.area.width {
            let cell = &buf[(x, y)];
            let colours = (cell.fg, cell.bg);
            if Some(colours) != run_colours {
                if let Some((fg, bg)) = run_colours {
                    let _ = write!(
                        out,
                        "<span style=\"color:{};background-color:{}\">{}</span>",
                        css(fg),
                        css(bg),
                        escape(&run)
                    );
                }
                run.clear();
                run_colours = Some(colours);
            }
            run.push_str(cell.symbol());
        }
        if let Some((fg, bg)) = run_colours {
            let _ = write!(
                out,
                "<span style=\"color:{};background-color:{}\">{}</span>",
                css(fg),
                css(bg),
                escape(&run)
            );
        }
        out.push('\n');
    }
    format!("<pre>{out}</pre>")
}

fn render(app: &App, width: u16, height: u16) -> String {
    let mut buf = Buffer::empty(Rect::new(0, 0, width, height));
    app.render_into(buf.area, &mut buf);
    frame_html(&buf)
}

/// A rendered frame inside terminal-window chrome, so a page showing two
/// of them side by side reads as two windows rather than one wide one.
fn window(title: &str, body: &str) -> String {
    format!(
        "<div class=\"win\"><div class=\"bar\">{}</div><div class=\"screen\">{}</div></div>",
        escape(title),
        body
    )
}

// --- The reduction proposals -------------------------------------------
//
// Everything below this line is *drawn*, not rendered: it is a proposal
// for a calmer frame than the one the crate builds today, and the crate
// does not build it yet. Kept honestly separate from the rendered frames
// above, because a drawn mockup that promises a layout the code does not
// produce is exactly the trap this project keeps falling into.

const PROPOSAL_WIDTH: usize = 62;

/// Pads `s` to `width` display columns. The box-drawing and block
/// characters used here are all single-width, so counting `chars` rather
/// than bytes is enough - `len()` would count the three bytes of every
/// `─` and shred the alignment.
fn pad(s: &str, width: usize) -> String {
    let n = s.chars().count();
    if n >= width {
        s.chars().take(width).collect()
    } else {
        format!("{s}{}", " ".repeat(width - n))
    }
}

/// Proposal A: the same furniture, trimmed. Single-line box instead of
/// double, three status fields instead of five, the two live bands
/// instead of six labelled rows plus a stage axis, four function keys
/// instead of seven.
fn trimmed(role: &str, band: &str, status: &str, waterfall: &[&str], lines: &[&str]) -> String {
    let inner = PROPOSAL_WIDTH - 2;
    let left = " modem ";
    let right = format!(" {role}  {band} ");
    let rule = "─".repeat(PROPOSAL_WIDTH - left.chars().count() - right.chars().count() - 2);
    let mut out = format!("┌{left}{rule}{right}┐\n");
    out.push_str(&format!("│{}│\n", pad(&format!(" {status}"), inner)));
    out.push_str(&format!("├{}┤\n", "─".repeat(inner)));
    for row in waterfall {
        out.push_str(&format!("│{}│\n", pad(&format!(" {row}"), inner)));
    }
    out.push_str(&format!("├{}┤\n", "─".repeat(inner)));
    for line in lines {
        out.push_str(&format!("│{}│\n", pad(&format!(" {line}"), inner)));
    }
    out.push_str(&format!("└{}┘\n", "─".repeat(inner)));
    out.push_str(" F3 dial   F4 answer   F6 colour   F10 hang up\n");
    out
}

/// Proposal B: no box at all. Two rules and whitespace carry the
/// structure the box was carrying.
fn stripped(role: &str, band: &str, status: &str, waterfall: &[&str], lines: &[&str]) -> String {
    let head = format!(
        "modem{}{role}  {band}",
        " ".repeat(PROPOSAL_WIDTH - 5 - role.chars().count() - band.chars().count() - 2)
    );
    let mut out = format!("{head}\n\n{status}\n\n");
    for row in waterfall {
        out.push_str(&format!("{row}\n"));
    }
    out.push('\n');
    for line in lines {
        out.push_str(&format!("{line}\n"));
    }
    out.push_str("\nF3 dial   F4 answer   F6 colour   F10 hang up\n");
    out
}

const WF_ORIGINATE: [&str; 2] = [
    "2225 \u{2581}\u{2582}\u{2583}\u{2585}\u{2587}\u{2588}\u{2587}\u{2585}\u{2583}\u{2582}\u{2581}\u{2581}\u{2582}\u{2583}\u{2585}\u{2587}\u{2588}\u{2587}\u{2585}\u{2583}\u{2582}\u{2581}\u{2581}\u{2582}\u{2583}\u{2585}\u{2587}\u{2588}\u{2587}\u{2585}\u{2583}\u{2582}\u{2581}\u{2581}\u{2582}\u{2583}\u{2585}\u{2587}\u{2588}\u{2587}\u{2585}\u{2583}\u{2582}\u{2581}\u{2581}\u{2582}\u{2583}\u{2585}\u{2587}\u{2588}\u{2587}\u{2585}\u{2583}",
    "1270 \u{2588}\u{2588}\u{2588}\u{2581}\u{2588}\u{2588}\u{2588}\u{2581}\u{2581}\u{2588}\u{2588}\u{2588}\u{2581}\u{2588}\u{2581}\u{2581}\u{2588}\u{2588}\u{2588}\u{2588}\u{2581}\u{2588}\u{2588}\u{2588}\u{2581}\u{2588}\u{2588}\u{2588}\u{2581}\u{2581}\u{2588}\u{2588}\u{2588}\u{2581}\u{2588}\u{2581}\u{2581}\u{2588}\u{2588}\u{2588}\u{2581}\u{2588}\u{2588}\u{2588}\u{2581}\u{2581}\u{2588}\u{2588}\u{2588}\u{2581}\u{2588}\u{2581}\u{2581}",
];

fn proposals() -> (String, String) {
    let origin_lines = [
        "ATDT01234567890",
        "CONNECT 300",
        "hello from the other side",
        "> took you long enough_",
    ];
    let answer_lines = [
        "ATA",
        "CONNECT 300",
        "> hello from the other side",
        "took you long eno_",
    ];

    let a = format!(
        "<div class=\"row\">{}{}</div>",
        window(
            "originate",
            &format!(
                "<pre class=\"drawn\">{}</pre>",
                escape(&trimmed(
                    "originate",
                    "1270/1070",
                    "\u{25CF} carrier   300 baud   00:01:23",
                    &WF_ORIGINATE,
                    &origin_lines,
                ))
            ),
        ),
        window(
            "answer",
            &format!(
                "<pre class=\"drawn\">{}</pre>",
                escape(&trimmed(
                    "answer",
                    "2225/2025",
                    "\u{25CF} carrier   300 baud   00:01:23",
                    &WF_ORIGINATE,
                    &answer_lines,
                ))
            ),
        ),
    );

    let b = format!(
        "<div class=\"row\">{}{}</div>",
        window(
            "originate",
            &format!(
                "<pre class=\"drawn\">{}</pre>",
                escape(&stripped(
                    "originate",
                    "1270/1070",
                    "\u{25CF} carrier   00:01:23",
                    &WF_ORIGINATE,
                    &origin_lines,
                ))
            ),
        ),
        window(
            "answer",
            &format!(
                "<pre class=\"drawn\">{}</pre>",
                escape(&stripped(
                    "answer",
                    "2225/2025",
                    "\u{25CF} carrier   00:01:23",
                    &WF_ORIGINATE,
                    &answer_lines,
                ))
            ),
        ),
    );

    (a, b)
}

fn main() {
    let wired = WiredTransport::new(8000);
    let (proposal_a, proposal_b) = proposals();

    // Two windows, side by side, one machine - as the crate renders it
    // today, in the new default phosphor. Both windows are fed the same
    // real mixed audio the call produced - the shared-air argument the
    // split layout makes explicitly (rule 1) is just as true of two
    // separate single-pane windows watching the same demo call.
    let (a, b, audio) = call();
    let mut left = App::single(a, &wired, Theme::default());
    let mut right = App::single(b, &wired, Theme::default());
    left.push_samples(&audio);
    right.push_samples(&audio);
    let side_by_side = format!(
        "<div class=\"row\">{}{}</div>",
        window("originate", &render(&left, 72, 22)),
        window("answer", &render(&right, 72, 22)),
    );

    // One window, both ends in it - the split layout the crate builds.
    let (a3, b3, audio3) = call();
    let mut split = App::split(a3, b3, &wired, Theme::default());
    split.push_samples(&audio3);
    let split_window = window("split screen", &render(&split, 100, 24));

    // The three phosphors, default first.
    let (a4, _, audio4) = call();
    let (a5, _, audio5) = call();
    let (a6, _, audio6) = call();
    let mut app4 = App::single(a4, &wired, Theme::White);
    let mut app5 = App::single(a5, &wired, Theme::Green);
    let mut app6 = App::single(a6, &wired, Theme::Amber);
    app4.push_samples(&audio4);
    app5.push_samples(&audio5);
    app6.push_samples(&audio6);
    let themes = format!(
        "<div class=\"row\">{}{}{}</div>",
        window("white - the default", &render(&app4, 62, 16)),
        window("green", &render(&app5, 62, 16)),
        window("amber", &render(&app6, 62, 16)),
    );

    print!(
        "{}",
        page(
            &proposal_a,
            &proposal_b,
            &side_by_side,
            &split_window,
            &themes
        )
    );
}

fn page(
    proposal_a: &str,
    proposal_b: &str,
    side_by_side: &str,
    split: &str,
    themes: &str,
) -> String {
    format!(
        r#"<!doctype html>
<html lang="en-GB">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>modem - terminal layouts</title>
<style>
  :root {{
    --ground: #0A0A0A;
    --white: #E8E8D8;
    --amber: #FFB000;
    --dim: #6b6459;
  }}
  * {{ box-sizing: border-box; }}
  body {{
    margin: 0;
    padding: 3rem 2rem 5rem;
    background: #050505;
    color: var(--white);
    font-family: "DejaVu Sans Mono", "Cascadia Mono", "Consolas", monospace;
  }}
  h1 {{ font-size: 1.4rem; font-weight: normal; letter-spacing: 0.3em; margin: 0 0 0.4rem }}
  h2 {{ font-size: 0.85rem; font-weight: normal; letter-spacing: 0.22em; color: var(--dim);
       text-transform: uppercase; margin: 3.5rem 0 0.3rem }}
  p  {{ color: #8d877c; font-size: 0.82rem; max-width: 60rem; line-height: 1.55; margin: 0 0 1.2rem }}
  strong {{ color: var(--white); font-weight: normal }}
  /* No wrapping: two windows side by side stay side by side, and the row
     scrolls sideways rather than stacking one above the other. */
  .row {{ display: flex; gap: 1.4rem; flex-wrap: nowrap; align-items: flex-start;
          overflow-x: auto; padding-bottom: 0.6rem }}
  .win {{ border: 1px solid #241f18; border-radius: 6px; overflow: hidden; flex: 0 0 auto;
          background: var(--ground); box-shadow: 0 18px 45px rgba(0,0,0,.75) }}
  .bar {{ background: #13110d; border-bottom: 1px solid #241f18; color: #6b6459;
          font-size: 0.66rem; letter-spacing: 0.18em; padding: 0.42rem 0.8rem }}
  .screen {{ position: relative; padding: 0.85rem 1rem; background: var(--ground) }}
  /* The scanlines and the bloom are the page's own CRT treatment, not
     something the terminal renders - a real terminal supplies its own. */
  .screen::after {{
    content: ""; position: absolute; inset: 0; pointer-events: none;
    background: repeating-linear-gradient(
      to bottom, rgba(0,0,0,.30) 0 1px, rgba(0,0,0,0) 1px 3px);
  }}
  pre {{ margin: 0; font: inherit; font-size: 0.75rem; line-height: 1.16 }}
  pre.drawn {{ color: var(--white); text-shadow: 0 0 6px rgba(232,232,216,.30) }}
  footer {{ margin-top: 5rem; color: #443f38; font-size: 0.75rem }}
</style>
</head>
<body>
<h1>modem</h1>
<p>White phosphor is now the default; <strong>F6</strong> cycles white, green, amber. The two proposals at the top are <strong>drawn</strong> - the crate does not build them yet. Everything below them is <strong>rendered by the real crate</strong>, with the two ends genuinely connected to each other and the chat text crossing that link as Bell 103 FSK at 300 baud.</p>

<h2>Proposal A - trimmed</h2>
<p>Single-line box instead of double. Three status fields instead of five. The two live bands instead of six labelled rows plus a stage axis. Four function keys instead of seven - only the ones that do something. Two windows, side by side, one machine.</p>
{proposal_a}

<h2>Proposal B - stripped</h2>
<p>No box at all. Whitespace and two rules carry what the box was carrying. Quieter still, and further from the 1990s comms-package register.</p>
{proposal_b}

<h2>Now - what the crate renders today</h2>
<p>For comparison, the same call in the current frame. The waterfall rows are empty because Task 16 builds the FFT that fills them. The call runs full duplex: under half duplex an idle end currently transmits silence, the far end reads that as carrier loss, and the first turn hand-over hangs the call up.</p>
{side_by_side}

<h2>The two earlier proposals, for the record</h2>
<p>Drawn, not rendered, and not being taken - kept so the comparison is on file.</p>
{proposal_a}
{proposal_b}

<h2>One window, both ends in it</h2>
<p>The split layout the crate also builds, sharing one spectrum between the panes.</p>
{split}

<h2>The three phosphors</h2>
{themes}

<footer>modem is a DBHQ experiment</footer>
</body>
</html>
"#
    )
}
