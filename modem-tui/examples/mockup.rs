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
fn pump(a: &mut Pane, b: &mut Pane, blocks: usize) {
    const BLOCK: usize = 256;
    let dt = Duration::from_secs_f64(BLOCK as f64 / 8000.0);
    let mut from_a = [0.0f32; BLOCK];
    let mut from_b = [0.0f32; BLOCK];
    for _ in 0..blocks {
        a.session_mut().process_out(&mut from_a);
        b.session_mut().process_out(&mut from_b);
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
    done: impl Fn(&Pane, &Pane) -> bool,
) {
    for _ in 0..cap {
        if done(a, b) {
            return;
        }
        pump(a, b, 1);
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
/// from.
fn call() -> (Pane, Pane) {
    let mut a = pane(Role::Originate);
    let mut b = pane(Role::Answer);

    a.type_line("ATDT01234567890");
    b.type_line("ATA");
    pump_until(&mut a, &mut b, 3000, "both ends to connect", |a, b| {
        a.carrier() && b.carrier() && a.history().iter().any(|l| l.contains("CONNECT"))
    });

    b.session_mut().send(b"hello from the other side\n");
    pump_until(
        &mut a,
        &mut b,
        3000,
        "the far end's line to arrive",
        |a, _| a.history().iter().any(|l| l.contains("other side")),
    );

    // A few seconds on the clock, so the elapsed field reads as a call in
    // progress rather than one that just started.
    pump(&mut a, &mut b, 800);

    for c in "took you long enough".chars() {
        a.feed_char(c);
    }
    pump(&mut a, &mut b, 20);

    (a, b)
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
/// sharing a colour rather than one per cell - a 100x24 frame is 2400
/// cells and a span each makes the page four times the size for no
/// visible difference.
fn frame_html(buf: &Buffer) -> String {
    let mut out = String::new();
    for y in 0..buf.area.height {
        let mut run = String::new();
        let mut run_colour: Option<Color> = None;
        for x in 0..buf.area.width {
            let cell = &buf[(x, y)];
            let colour = cell.fg;
            if Some(colour) != run_colour {
                if let Some(prev) = run_colour {
                    let _ = write!(
                        out,
                        "<span style=\"color:{}\">{}</span>",
                        css(prev),
                        escape(&run)
                    );
                }
                run.clear();
                run_colour = Some(colour);
            }
            run.push_str(cell.symbol());
        }
        if let Some(prev) = run_colour {
            let _ = write!(
                out,
                "<span style=\"color:{}\">{}</span>",
                css(prev),
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

fn main() {
    let wired = WiredTransport::new(8000);

    // Two windows, side by side, one machine. Each is an ordinary
    // single-pane frame - the same layout a second machine would run.
    let (a, b) = call();
    let left = App::single(a, &wired, Theme::Amber);
    let right = App::single(b, &wired, Theme::Amber);
    let side_by_side = format!(
        "<div class=\"row\">{}{}</div>",
        window("modem - originate", &render(&left, 100, 24)),
        window("modem - answer", &render(&right, 100, 24)),
    );

    // One window, one end, connected to a second machine.
    let (a2, _) = call();
    let single = App::single(a2, &wired, Theme::Amber);
    let one_window = window("modem", &render(&single, 100, 24));

    // One window, both ends in it - the split layout the crate already
    // builds, shown for comparison.
    let (a3, b3) = call();
    let split = App::split(a3, b3, &wired, Theme::Amber);
    let split_window = window("modem - split screen", &render(&split, 100, 24));

    // The same single-pane frame in the other two phosphors.
    let (a4, _) = call();
    let green = App::single(a4, &wired, Theme::Green);
    let (a5, _) = call();
    let white = App::single(a5, &wired, Theme::White);
    let themes = format!(
        "<div class=\"row\">{}{}</div>",
        window("green", &render(&green, 100, 24)),
        window("white", &render(&white, 100, 24)),
    );

    print!(
        "{}",
        page(&side_by_side, &one_window, &split_window, &themes)
    );
}

fn page(side_by_side: &str, one_window: &str, split: &str, themes: &str) -> String {
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
    --amber: #FFB000;
    --dim: #805800;
  }}
  * {{ box-sizing: border-box; }}
  body {{
    margin: 0;
    padding: 3rem 2rem 5rem;
    background: #050505;
    color: var(--amber);
    font-family: "DejaVu Sans Mono", "Cascadia Mono", "Consolas", monospace;
  }}
  h1 {{ font-size: 1.4rem; font-weight: normal; letter-spacing: 0.3em; margin: 0 0 0.4rem }}
  h2 {{ font-size: 0.85rem; font-weight: normal; letter-spacing: 0.22em; color: var(--dim);
       text-transform: uppercase; margin: 3.5rem 0 0.3rem }}
  p  {{ color: #9a8a6a; font-size: 0.82rem; max-width: 62rem; line-height: 1.55; margin: 0 0 1.2rem }}
  .row {{ display: flex; gap: 1.5rem; flex-wrap: wrap; align-items: flex-start }}
  .win {{ border: 1px solid #2a2213; border-radius: 6px; overflow: hidden;
          background: var(--ground); box-shadow: 0 18px 45px rgba(0,0,0,.75) }}
  .bar {{ background: #16120a; border-bottom: 1px solid #2a2213; color: #7d6c4a;
          font-size: 0.68rem; letter-spacing: 0.18em; padding: 0.45rem 0.8rem }}
  .screen {{ position: relative; padding: 0.9rem 1rem; background: var(--ground) }}
  /* The scanlines and the bloom are the page's own CRT treatment, not
     something the terminal renders - a real terminal supplies its own. */
  .screen::after {{
    content: ""; position: absolute; inset: 0; pointer-events: none;
    background: repeating-linear-gradient(
      to bottom, rgba(0,0,0,.32) 0 1px, rgba(0,0,0,0) 1px 3px);
  }}
  pre {{
    margin: 0; font: inherit; font-size: 0.78rem; line-height: 1.16;
    text-shadow: 0 0 6px rgba(255,176,0,.35);
  }}
  footer {{ margin-top: 5rem; color: #4d4433; font-size: 0.75rem }}
  a {{ color: var(--dim) }}
</style>
</head>
<body>
<h1>modem</h1>
<p>Every frame below is rendered by the real crate, not drawn. The two ends are genuinely connected to each other: one end's modulated audio is fed straight into the other's demodulator, and the text you can read crossed that link as Bell 103 FSK at 300 baud. The scanlines and the glow are this page's own CRT treatment, not something the terminal draws.</p>
<p>Two things are not finished. <strong>The waterfall is empty</strong> - Task 16 builds the FFT that fills it, and this is the hole it drops into. <strong>The call runs full duplex</strong>, because under half duplex an idle end currently transmits silence, the far end reads that as carrier loss and the first turn hand-over hangs the call up. Full duplex is what these frames were captured over and the status line says so.</p>

<h2>One machine, two windows side by side</h2>
<p>How the demo gets filmed. Both ends on one desk, one window each, the audio audible.</p>
{side_by_side}

<h2>Two devices, one window each</h2>
<p>The real product. Full width, the overture stage labels along the waterfall axis.</p>
{one_window}

<h2>One window, both ends in it</h2>
<p>The split layout the crate builds today - the same two ends inside a single window, sharing one spectrum. Shown for comparison with the two-window arrangement above.</p>
{split}

<h2>The other two phosphors</h2>
<p>F6 cycles amber, green, white.</p>
{themes}

<footer>modem is a DBHQ experiment</footer>
</body>
</html>
"#
    )
}
