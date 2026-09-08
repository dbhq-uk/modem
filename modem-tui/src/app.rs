//! The comms-package application: single-pane and split-screen layouts
//! over the same widget set, tied to a real or wired [`Transport`].
//!
//! # The three non-negotiable rules, and where each is enforced
//!
//! 1. **Both panes render the same spectrum.** [`App`] holds exactly one
//!    [`Spectrum`] (`self.spectrum`) and [`App::render_into`] passes the
//!    same `&Spectrum` to both panes' [`crate::waterfall::render`] calls
//!    when split - there is no per-pane spectrum field to drift out of
//!    sync. See `split_screen_panes_render_identical_waterfalls` and its
//!    mutation proof below.
//! 2. **`[DEMO MODE]` shows whenever `Transport::is_acoustic()` is
//!    false.** [`App::single`] and [`App::split`] are the only ways to
//!    build an `App`, and both take `&dyn Transport` and store
//!    `!transport.is_acoustic()` - there is no constructor that accepts a
//!    caller-supplied `bool`, so a caller cannot special-case "but this
//!    one's actually `WiredTransport`" even if tempted to. See
//!    `demo_mode_is_read_from_the_transport_trait_method` and its
//!    mutation proof.
//! 3. **Below about 80 columns, split screen is unusable.** [`decide_layout`]
//!    is the one place width is compared against [`MIN_SPLIT_COLUMNS`],
//!    and [`App::render_into`] refuses to lay out two panes at all below
//!    it - see `split_below_eighty_columns_shows_a_message_not_a_broken_layout`
//!    and its mutation proof.

use std::path::PathBuf;
use std::time::Duration;

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::widgets::Widget;

use modem_audio::Transport;

use crate::directory::Directory;
use crate::draw;
use crate::frame::{self, Title};
use crate::pane::Pane;
use crate::spectrum::Spectrum;
use crate::theme::Theme;
use crate::waterfall;

/// Split screen refuses to lay out below this width - see the design
/// spec: "Below about 80 columns, split screen is unusable."
pub const MIN_SPLIT_COLUMNS: u16 = 80;

/// Which layout the caller asked for - a standing choice (a CLI flag,
/// eventually), not something that flips on its own based on terminal
/// size. [`decide_layout`] is what turns this into an actual
/// [`LayoutMode`] for a given size.
///
/// **Split is the default** (Dan, 8 Sep 2026): both ends on one machine
/// is what somebody sees first, and single pane is what they run once
/// they have a second machine to point it at. Both are first-class - the
/// default only decides which one comes up with no flag.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum RequestedLayout {
    Single,
    #[default]
    Split,
}

/// What actually gets drawn this frame, after checking the requested
/// layout against the real terminal size.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LayoutMode {
    Single,
    Split,
    /// Split was requested but the terminal is narrower than
    /// [`MIN_SPLIT_COLUMNS`] - rendered as a message, not a squeezed or
    /// overlapping pair of panes.
    TooNarrow,
}

/// Turns a requested layout and a real terminal width into what to
/// actually draw. The one and only place [`MIN_SPLIT_COLUMNS`] is
/// compared against a real width - see this module's own doc, rule 3.
pub fn decide_layout(requested: RequestedLayout, area: Rect) -> LayoutMode {
    match requested {
        RequestedLayout::Single => LayoutMode::Single,
        RequestedLayout::Split => {
            if area.width < MIN_SPLIT_COLUMNS {
                LayoutMode::TooNarrow
            } else {
                LayoutMode::Split
            }
        }
    }
}

/// The comms-package application. See this module's own doc for the
/// three rules this type exists to enforce.
pub struct App {
    requested: RequestedLayout,
    panes: Vec<Pane>,
    focus: usize,
    demo_mode: bool,
    spectrum: Spectrum,
    /// Raw samples accumulated by [`App::push_samples`] until there are
    /// enough for one FFT - see that method's own doc.
    sample_buffer: Vec<f32>,
    fft_size: usize,
    theme: Theme,
    /// The dialling directory, shown as an overlay over the focused pane
    /// while `Some` - see [`DirectoryOverlay`]'s own doc. `None` the rest
    /// of the time; nothing is loaded from disk until `F2` is actually
    /// pressed (see [`App::handle_key`]), so building an `App` never
    /// touches the filesystem on its own.
    directory_overlay: Option<DirectoryOverlay>,
}

/// The dialling directory's own overlay state - the loaded directory, the
/// path it was loaded from (so the empty-directory message can say where
/// it looked), and which entry is currently highlighted.
///
/// `F2` (see [`App::handle_key`]) always loads fresh from disk when
/// opening - a hand-edited file is exactly the kind of thing somebody
/// changes between calls, and re-reading it on every open is what makes
/// that edit visible without restarting the whole application.
struct DirectoryOverlay {
    directory: Directory,
    path: PathBuf,
    /// An index into `directory.entries()` - `0` when the directory is
    /// empty, same as everywhere else in this crate's own convention of
    /// never letting an empty collection produce an out-of-range index.
    selected: usize,
}

/// The next selection index, wrapping from the last entry back to the
/// first. `len == 0` (no entries loaded, e.g. a missing file) always
/// yields `0` rather than computing a modulus by zero.
fn next_selection(selected: usize, len: usize) -> usize {
    if len == 0 {
        0
    } else {
        (selected + 1) % len
    }
}

/// The previous selection index, wrapping from the first entry to the
/// last - see [`next_selection`]'s own doc for the `len == 0` case.
fn prev_selection(selected: usize, len: usize) -> usize {
    if len == 0 {
        0
    } else {
        (selected + len - 1) % len
    }
}

impl App {
    /// One pane, connected to another machine - the real product. `demo_mode`
    /// is read from `transport.is_acoustic()`, never supplied directly -
    /// see this module's own doc, rule 2.
    pub fn single(pane: Pane, transport: &dyn Transport, theme: Theme) -> Self {
        let sample_rate = transport.sample_rate();
        App {
            requested: RequestedLayout::Single,
            panes: vec![pane],
            focus: 0,
            demo_mode: !transport.is_acoustic(),
            spectrum: Spectrum::new(sample_rate),
            sample_buffer: Vec::new(),
            fft_size: modem_core::analyse::fft_size_for(sample_rate),
            theme,
            directory_overlay: None,
        }
    }

    /// Both ends on one machine, for demonstration. `demo_mode` is read
    /// from `transport.is_acoustic()`, never supplied directly - see this
    /// module's own doc, rule 2.
    pub fn split(a: Pane, b: Pane, transport: &dyn Transport, theme: Theme) -> Self {
        let sample_rate = transport.sample_rate();
        App {
            requested: RequestedLayout::Split,
            panes: vec![a, b],
            focus: 0,
            demo_mode: !transport.is_acoustic(),
            spectrum: Spectrum::new(sample_rate),
            sample_buffer: Vec::new(),
            fft_size: modem_core::analyse::fft_size_for(sample_rate),
            theme,
            directory_overlay: None,
        }
    }

    pub fn demo_mode(&self) -> bool {
        self.demo_mode
    }

    pub fn focus(&self) -> usize {
        self.focus
    }

    pub fn theme(&self) -> Theme {
        self.theme
    }

    pub fn panes(&self) -> &[Pane] {
        &self.panes
    }

    pub fn spectrum(&self) -> &Spectrum {
        &self.spectrum
    }

    /// Pushes a whole magnitude column onto the one spectrum both panes
    /// render from - see this module's own doc, rule 1: there is
    /// deliberately no per-pane equivalent. Mostly useful for tests and
    /// anything that has already run `modem_core::analyse::magnitudes`
    /// itself; [`App::push_samples`] is the usual way in from raw audio.
    pub fn push_spectrum_column(&mut self, column: Vec<f32>) {
        self.spectrum.push(column);
    }

    /// Feeds raw samples in, accumulating until there are enough for one
    /// FFT (`fft_size_for(sample_rate)`, decided once at construction from
    /// the transport's own rate) and pushing a column each time the
    /// buffer fills - the waterfall's link back to real audio. A caller
    /// with acoustic samples, or (as the mockup example does) the actual
    /// sample blocks a wired demo call is exchanging, can feed them here
    /// unmodified; the accumulation and windowing is this crate's job, not
    /// the caller's.
    pub fn push_samples(&mut self, samples: &[f32]) {
        self.sample_buffer.extend_from_slice(samples);
        while self.sample_buffer.len() >= self.fft_size {
            let window: Vec<f32> = self.sample_buffer.drain(0..self.fft_size).collect();
            let mut mags = vec![0.0f32; self.fft_size / 2];
            modem_core::analyse::magnitudes(&window, &mut mags);
            self.spectrum.push(mags);
        }
    }

    /// Drives every pane's own `Session` through one real
    /// [`Transport::run`] call - the binary's event loop uses this for
    /// `--acoustic` (a real [`modem_audio::CpalTransport`]), where the
    /// device's own ring-buffer pacing must not be bypassed.
    ///
    /// `Transport::run` requires a genuinely contiguous `&mut [Session]`
    /// (see its own doc), and a [`Pane`] owns its `Session` privately -
    /// there is deliberately no `panes_mut()` escape hatch that would let
    /// a caller reach in and drive a pane's session directly, bypassing
    /// whatever else this type wants to guarantee about it. So this
    /// briefly swaps each pane's real session out for a throwaway
    /// placeholder (a fresh `Session::new` from that same pane's own
    /// `Config` - never read from or written to, so its own state does
    /// not matter), gathers the real ones into the slice the trait
    /// requires, runs the transport exactly once, and hands each one
    /// back. Nothing observes a pane's session through any other method
    /// while it is briefly swapped out - the whole exchange happens
    /// within this one call.
    ///
    /// Does not feed the waterfall: `Transport::run`'s own return value
    /// (`RunStats`) reports block counts, never the actual samples
    /// exchanged, so there is nothing here to hand `push_samples`. See
    /// [`App::step_wired`] for the demo path, which drives the two
    /// panes' sessions directly and can see the real audio.
    pub fn run_transport(
        &mut self,
        transport: &mut dyn Transport,
    ) -> Result<modem_audio::transport::RunStats, modem_audio::transport::TransportError> {
        let mut ends: Vec<modem_core::session::Session> = self
            .panes
            .iter_mut()
            .map(|p| p.take_session(modem_core::session::Session::new(p.config())))
            .collect();
        let result = transport.run(&mut ends);
        for (pane, session) in self.panes.iter_mut().zip(ends) {
            pane.restore_session(session);
        }
        result
    }

    /// Demo-only: cross-wires the two panes' own `Session`s directly in
    /// software for exactly one block - the same cross-wire
    /// `modem_audio::WiredTransport::step` performs, reimplemented here
    /// rather than reached through `Transport::run`/`WiredTransport`
    /// because that trait method's own `RunStats` return never exposes
    /// the samples it just produced (see `run_transport`'s own doc), and
    /// the whole point of the wired demo path is feeding those very
    /// samples to the waterfall - exactly what `examples/mockup.rs`'s own
    /// `pump` function already does, tested and working, which this
    /// mirrors directly. `WiredTransport` itself is still constructed by
    /// the binary and used for its metadata (`is_acoustic`,
    /// `sample_rate`, `config_for`) - only its own `run`/`step` is
    /// bypassed here.
    ///
    /// Panics if this `App` does not have exactly two panes - the wired
    /// demo always cross-wires two ends; the binary's own startup check
    /// refuses `--single` without `--acoustic` before this is ever
    /// reached, for exactly this reason.
    pub fn step_wired(&mut self) {
        assert_eq!(
            self.panes.len(),
            2,
            "step_wired needs exactly two panes - the wired demo always cross-wires two ends"
        );
        const BLOCK: usize = 256;
        let mut from_a = [0.0f32; BLOCK];
        let mut from_b = [0.0f32; BLOCK];
        {
            let (first, second) = self.panes.split_at_mut(1);
            let a = first[0].session_mut();
            let b = second[0].session_mut();
            a.process_out(&mut from_a);
            b.process_out(&mut from_b);
            a.process_in(&from_b);
            b.process_in(&from_a);
        }
        let mix: Vec<f32> = from_a
            .iter()
            .zip(from_b.iter())
            .map(|(&x, &y)| (x + y).clamp(-1.0, 1.0))
            .collect();
        self.push_samples(&mix);
    }

    pub fn layout_mode(&self, area: Rect) -> LayoutMode {
        decide_layout(self.requested, area)
    }

    /// Routes one key event to the focused pane, or to the app itself for
    /// the keys this crate already has enough to act on (`F2` open the
    /// dialling directory, `F7` swap focus, `F6` theme cycle, `F4`
    /// answer, `F10` hang up). `F1`/`F3`/`F5` are still chrome only -
    /// help and the standalone waterfall view need UI this task does not
    /// build.
    ///
    /// **While the directory overlay is open, every key goes to it
    /// instead of the pane** - see [`DirectoryOverlay`]'s own doc. Up and
    /// Down move the selection and wrap at both ends
    /// ([`next_selection`]/[`prev_selection`]); Enter dials the
    /// highlighted entry through [`Pane::type_line`] (so the AT layer
    /// sees a real `ATDT` command, not a special case) and closes the
    /// overlay; Esc and a second `F2` close it without dialling. Nothing
    /// else does anything while it is open - typing into the pane
    /// underneath while browsing the directory would be confusing, not
    /// useful.
    pub fn handle_key(&mut self, key: KeyEvent) {
        // Windows reports both press and release; Unix (without the
        // keyboard-enhancement protocol) reports only press. Acting on
        // anything but a press would double-feed every character typed
        // on Windows.
        if key.kind != KeyEventKind::Press {
            return;
        }

        if let Some(overlay) = &mut self.directory_overlay {
            match key.code {
                KeyCode::Down => {
                    overlay.selected =
                        next_selection(overlay.selected, overlay.directory.entries().len());
                }
                KeyCode::Up => {
                    overlay.selected =
                        prev_selection(overlay.selected, overlay.directory.entries().len());
                }
                KeyCode::Enter => {
                    // The command is built and the overlay is dropped
                    // before `feed_focused_line` runs, so nothing here
                    // can still be reading `overlay` once dialling
                    // starts touching the focused pane.
                    let dial = overlay
                        .directory
                        .entries()
                        .get(overlay.selected)
                        .map(|entry| format!("ATDT{}", entry.number));
                    self.directory_overlay = None;
                    if let Some(cmd) = dial {
                        self.feed_focused_line(&cmd);
                    }
                }
                KeyCode::Esc | KeyCode::F(2) => {
                    self.directory_overlay = None;
                }
                _ => {}
            }
            return;
        }

        match key.code {
            KeyCode::F(2) => {
                let (directory, path) = Directory::load();
                self.directory_overlay = Some(DirectoryOverlay {
                    directory,
                    path,
                    selected: 0,
                });
            }
            KeyCode::F(7) => {
                if self.panes.len() == 2 {
                    self.focus = 1 - self.focus;
                }
            }
            KeyCode::F(6) => self.theme = self.theme.next(),
            KeyCode::F(4) => {
                if let Some(pane) = self.panes.get_mut(self.focus) {
                    pane.session_mut().answer();
                }
            }
            KeyCode::F(10) => {
                if let Some(pane) = self.panes.get_mut(self.focus) {
                    pane.session_mut().hangup();
                }
            }
            KeyCode::Enter => self.feed_focused('\r'),
            KeyCode::Backspace => {
                if let Some(pane) = self.panes.get_mut(self.focus) {
                    pane.backspace();
                }
            }
            KeyCode::Char(c) => self.feed_focused(c),
            _ => {}
        }
    }

    fn feed_focused(&mut self, c: char) {
        if let Some(pane) = self.panes.get_mut(self.focus) {
            pane.feed_char(c);
        }
    }

    /// Types a whole line into the focused pane through [`Pane::type_line`],
    /// one character at a time through the real `AtProcessor`, exactly as
    /// a person would type it. Used by the directory overlay's Enter
    /// handling so dialling a directory entry is indistinguishable, from
    /// the AT layer's own point of view, from somebody typing `ATDT`
    /// themselves.
    fn feed_focused_line(&mut self, line: &str) {
        if let Some(pane) = self.panes.get_mut(self.focus) {
            pane.type_line(line);
        }
    }

    /// Advances every pane's clock by `dt` - see [`Pane::tick`].
    pub fn tick(&mut self, dt: Duration) {
        for pane in &mut self.panes {
            pane.tick(dt);
        }
    }

    /// Renders this app into `buf` within `area`. Never panics at any
    /// `area`, including zero-sized - see `frame::plan_rows`'s own doc
    /// for the mechanism, and the size-sweep test below for the actual
    /// proof.
    pub fn render_into(&self, area: Rect, buf: &mut Buffer) {
        // The CRT ground, laid down first so every widget only has to
        // draw its own foreground - a period terminal's background was
        // never the terminal emulator's own default, it was this near-
        // black phosphor ground.
        draw::fill(buf, area, ' ', Style::default().bg(crate::theme::GROUND));
        // `render_frame` hands back the focused pane's own content rect
        // from the one `FrameLayout` it already computed - see `frame`'s
        // own module doc on why there is only ever one such computation
        // per frame - so the overlay draws into exactly the area the
        // pane it covers actually occupies, never a second, possibly
        // divergent, recomputation of the same layout.
        let focused_rect = match self.layout_mode(area) {
            LayoutMode::TooNarrow => {
                self.render_too_narrow(area, buf);
                None
            }
            LayoutMode::Single => Some(self.render_frame(area, buf, false)),
            LayoutMode::Split => Some(self.render_frame(area, buf, true)),
        };
        if let (Some(overlay), Some(rect)) = (&self.directory_overlay, focused_rect) {
            self.render_directory_overlay(overlay, rect, buf);
        }
    }

    /// Draws the dialling directory over `area` - the focused pane's own
    /// content rect, so the overlay never spills into the other pane in
    /// split mode. Bounds-checked the same way every other widget in this
    /// crate is (through [`draw::text`]/[`draw::fill`]), so this never
    /// panics at any size, including one smaller than the overlay's own
    /// content - see the size-sweep test below.
    fn render_directory_overlay(&self, overlay: &DirectoryOverlay, area: Rect, buf: &mut Buffer) {
        if area.width == 0 || area.height == 0 {
            return;
        }
        let bright = Style::default().fg(self.theme.bright());
        let dim = Style::default().fg(self.theme.dim());

        // The overlay is a panel drawn *inside* the frame, inset by one
        // cell on every side and carrying its own single-line border.
        // Writing at the frame's own coordinates put the header and the
        // "no entries" line straight over the double-line rules, so an
        // empty directory rendered with the box's own bars replaced by
        // text. A panel laid over chrome has to bring its own edges.
        // Exactly the focused pane's content rect - it already sits
        // inside the frame's rails, so insetting further leaves a strip
        // of the waterfall's own labels showing down the left of the
        // panel.
        let panel = area;
        if panel.width < 6 || panel.height < 3 {
            // Too small for a bordered panel: plain text in the area
            // given, rather than a broken box.
            draw::fill(buf, area, ' ', Style::default().bg(crate::theme::GROUND));
            draw::text(buf, area, area.left(), area.top(), "directory", bright);
            return;
        }
        draw::fill(buf, panel, ' ', Style::default().bg(crate::theme::GROUND));

        let header = if overlay.directory.skipped() > 0 {
            format!(
                " dialling directory ({} skipped) ",
                overlay.directory.skipped()
            )
        } else {
            " dialling directory ".to_string()
        };
        let span = panel.width.saturating_sub(2) as usize;
        let title: String = header.chars().take(span).collect();
        let rule = span - title.chars().count();
        draw::text(
            buf,
            panel,
            panel.left(),
            panel.top(),
            &format!("\u{250C}{title}{}\u{2510}", "\u{2500}".repeat(rule)),
            bright,
        );
        for row in 1..panel.height.saturating_sub(1) {
            let y = panel.top() + row;
            draw::cell(buf, panel, panel.left(), y, '\u{2502}', bright);
            draw::cell(
                buf,
                panel,
                panel.right().saturating_sub(1),
                y,
                '\u{2502}',
                bright,
            );
        }
        draw::text(
            buf,
            panel,
            panel.left(),
            panel.bottom().saturating_sub(1),
            &format!("\u{2514}{}\u{2518}", "\u{2500}".repeat(span)),
            bright,
        );

        // Everything below writes inside the panel's own border.
        let area = Rect {
            x: panel.left().saturating_add(1),
            y: panel.top().saturating_add(1),
            width: panel.width.saturating_sub(2),
            height: panel.height.saturating_sub(2),
        };
        let mut y = area.top();
        if overlay.directory.entries().is_empty() {
            // The one thing this format must never do is fail silently -
            // an empty overlay with no explanation reads as broken, not
            // as "there is nothing here yet".
            let msg = format!("no entries - looked in {}", overlay.path.display());
            draw::text(buf, area, area.left(), y, &msg, dim);
            return;
        }

        for (i, entry) in overlay.directory.entries().iter().enumerate() {
            if y >= area.bottom() {
                break;
            }
            let marker = if i == overlay.selected { '>' } else { ' ' };
            let line = if entry.note.is_empty() {
                format!("{marker} {}  {}", entry.name, entry.number)
            } else {
                format!("{marker} {}  {}  {}", entry.name, entry.number, entry.note)
            };
            let style = if i == overlay.selected { bright } else { dim };
            draw::text(buf, area, area.left(), y, &line, style);
            y = y.saturating_add(1);
        }
    }

    fn render_too_narrow(&self, area: Rect, buf: &mut Buffer) {
        let msg = format!(
            "Split screen needs at least {} columns (currently {})",
            MIN_SPLIT_COLUMNS, area.width
        );
        draw::text(buf, area, area.left(), area.top(), &msg, Style::default());
    }

    fn render_frame(&self, area: Rect, buf: &mut Buffer, split: bool) -> Rect {
        // The fkey bar is its own row below the outer frame, outside its
        // border - see the mockups. One row is reserved for it whenever
        // there is one to spare; on an area too short even for that, the
        // frame simply takes the whole thing and the bar is dropped
        // rather than the frame being squeezed to make room.
        let (frame_area, fkey_area) = if area.height > 1 {
            (
                Rect {
                    height: area.height - 1,
                    ..area
                },
                Some(Rect {
                    y: area.bottom() - 1,
                    height: 1,
                    ..area
                }),
            )
        } else {
            (area, None)
        };

        let pane_count = if split { 2 } else { 1 };
        let desired_waterfall_rows = if split {
            waterfall::DESIRED_ROWS
        } else {
            waterfall::DESIRED_ROWS + 1 // + the stage-axis footer row
        };
        let layout = frame::layout(frame_area, pane_count, desired_waterfall_rows);

        let titles = self.titles(split);
        let focused = if split { Some(self.focus) } else { None };
        frame::draw(
            buf,
            &layout,
            &titles,
            focused,
            Style::default().fg(self.theme.bright()),
            Style::default().fg(self.theme.dim()),
        );

        for (i, col) in layout.columns.iter().enumerate() {
            let Some(pane) = self.panes.get(i) else {
                continue;
            };
            let is_active = if split { i == self.focus } else { true };
            let style = if is_active || !split {
                Style::default().fg(self.theme.bright())
            } else {
                Style::default().fg(self.theme.dim())
            };
            self.render_pane_content(buf, *col, &layout.rows, pane, style, is_active, split);
        }

        if let Some(fkey_area) = fkey_area {
            self.render_fkey_bar(buf, fkey_area, split);
        }

        layout
            .columns
            .get(self.focus)
            .copied()
            .unwrap_or(frame_area)
    }

    fn titles(&self, split: bool) -> Vec<Title> {
        if !split {
            let pane = &self.panes[0];
            return vec![Title {
                left: "modem".to_string(),
                // No "Hz" - the row labels down the waterfall's left edge
                // are the same numbers in the same units, so the unit on
                // the title bar is three columns saying nothing.
                right: format!("{} \u{b7} {}", pane.role_name(), pane.band_label()),
            }];
        }
        self.panes
            .iter()
            .map(|pane| Title {
                left: pane.role_name().to_string(),
                right: pane.band_label(),
            })
            .collect()
    }

    #[allow(clippy::too_many_arguments)]
    fn render_pane_content(
        &self,
        buf: &mut Buffer,
        col: Rect,
        rows: &frame::RowPlan,
        pane: &Pane,
        style: Style,
        is_active: bool,
        split: bool,
    ) {
        if let Some(y) = rows.status {
            let text = status_text(pane, !split);
            draw::text(buf, col, col.left().saturating_add(1), y, &text, style);
        }

        if let Some(top) = rows.waterfall_top {
            let waterfall_area = Rect {
                y: top,
                height: rows.waterfall_rows,
                ..col
            };
            // Single-pane mode reserves exactly one row for the
            // stage-axis footer, when there is more than one row to
            // spare; split mode has no footer at all (see the design
            // spec's own split mockup) and gives every row to the bars.
            // Unlike Task 15's placeholder, the waterfall itself is not
            // capped to a fixed row count - more height is genuinely
            // better frequency resolution, not wasted space.
            let footer_rows: u16 = if !split && waterfall_area.height > 1 {
                1
            } else {
                0
            };
            let bar_rows = waterfall_area.height - footer_rows;
            let bar_area = Rect {
                height: bar_rows,
                ..waterfall_area
            };
            waterfall::render(
                buf,
                bar_area,
                &self.spectrum,
                style,
                Style::default().fg(self.theme.dim()),
            );
            if footer_rows > 0 {
                let footer_area = Rect {
                    y: waterfall_area.y + bar_rows,
                    height: footer_rows,
                    ..waterfall_area
                };
                waterfall::render_stage_axis(buf, footer_area, style);
            }
        }

        if let Some(top) = rows.terminal_top {
            let terminal_area = Rect {
                y: top,
                height: rows.terminal_rows,
                ..col
            };
            self.render_terminal(buf, terminal_area, pane, style, is_active);
        }
    }

    fn render_terminal(
        &self,
        buf: &mut Buffer,
        area: Rect,
        pane: &Pane,
        style: Style,
        show_prompt: bool,
    ) {
        let width = area.width as usize;
        if width == 0 || area.height == 0 {
            return;
        }
        let terminal_rows = area.height as usize;

        // Every line is wrapped to the pane's width before any of it is
        // placed, because `draw::text` clips: an unwrapped 90-character
        // line in a 68-column pane loses its tail with nothing on screen
        // saying so, and a chat client that silently eats the end of
        // what the far end said is worse than a denser status bar.
        let prompt = if show_prompt {
            let prefix = if pane.in_command_mode() { "" } else { "> " };
            wrap(&format!("{prefix}{}_", pane.composing()), width)
        } else {
            Vec::new()
        };

        // The prompt is what you are typing right now, so it keeps its
        // rows and history gives way - never the other way round.
        let prompt_rows = prompt.len().min(terminal_rows);
        let history_rows = terminal_rows - prompt_rows;

        let wrapped: Vec<String> = pane
            .history()
            .iter()
            .flat_map(|line| wrap(line, width))
            .collect();
        let start = wrapped.len().saturating_sub(history_rows);
        for (i, line) in wrapped[start..].iter().enumerate() {
            draw::text(buf, area, area.left(), area.top() + i as u16, line, style);
        }

        // A prompt longer than the pane is tall shows its tail: the
        // cursor has to stay visible, so it is the oldest rows that go.
        let first = prompt.len() - prompt_rows;
        for (i, line) in prompt[first..].iter().enumerate() {
            let y = area.top() + (history_rows + i) as u16;
            draw::text(buf, area, area.left(), y, line, style);
        }
    }

    fn render_fkey_bar(&self, buf: &mut Buffer, area: Rect, split: bool) {
        let base = if split {
            FKEY_BAR_SPLIT
        } else {
            FKEY_BAR_SINGLE
        };
        let style = Style::default().fg(self.theme.bright());

        // The badge outranks the bar. Drawing the bar first and letting
        // the badge land wherever is left is what produced a bare "["
        // at 76 columns - the keys are a reminder, the badge is the one
        // thing on screen saying this link is not acoustic (rule 2), so
        // when only one of them fits it is the badge that fits.
        let tag = "[DEMO MODE]";
        let tag_width = tag.chars().count() as u16 + 2;
        let bar_room = if self.demo_mode {
            area.width.saturating_sub(tag_width)
        } else {
            area.width
        };
        let bar: String = base.chars().take(bar_room as usize).collect();
        draw::text(buf, area, area.left(), area.top(), &bar, style);

        if self.demo_mode && area.width >= tag_width {
            let x = area.right().saturating_sub(tag.chars().count() as u16 + 1);
            draw::text(buf, area, x, area.top(), tag, style);
        }
    }
}

/// The function-key strip, single-pane. **Only keys [`App::handle_key`]
/// actually acts on.** F1 help, F3 dial (superseded by F2's own Enter-to-
/// dial) and F5 waterfall were all on this bar and none of them did
/// anything - help and the standalone waterfall view still have no UI.
/// F2 comes back here with Task 18's dialling directory, now that there
/// is a real number source and a real UI behind it. Advertising a dead
/// key costs width the frame needs to fit two windows side by side, and
/// this project's whole argument is that it does not claim things it is
/// not doing. The rest come back as their own features land.
const FKEY_BAR_SINGLE: &str = " F2 directory  F4 answer  F6 colour  F10 hang up";

/// The same strip when split, with focus swapping in place of the colour
/// cycle - both panes share one theme, so F6 has nothing pane-specific
/// to say here.
const FKEY_BAR_SPLIT: &str = " F2 directory  F4 answer  F7 swap focus  F10 hang up";

/// Hard-wraps `line` to `width` columns, the way a terminal does - no
/// word breaking, because a modem transcript is a character grid and a
/// word-wrapped one would not line up with what the far end sent. An
/// empty line still produces one row, so a blank line in the scrollback
/// stays a blank line rather than vanishing.
fn wrap(line: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return Vec::new();
    }
    let chars: Vec<char> = line.chars().collect();
    if chars.is_empty() {
        return vec![String::new()];
    }
    chars
        .chunks(width)
        .map(|chunk| chunk.iter().collect())
        .collect()
}

fn status_text(pane: &Pane, wide: bool) -> String {
    let carrier = if pane.carrier() {
        "\u{25CF} CARRIER"
    } else {
        "\u{25CB} NO CARRIER"
    };
    if wide {
        // Under `Duplex::Full` the turn label has no turn to name and
        // repeats the duplex mode instead (see `Pane::turn_or_state_label`),
        // so printing both fields renders "full duplex    full duplex".
        // Collapsing them keeps the field count honest rather than
        // padding the line with a value already on it.
        let duplex = pane.duplex_label();
        let turn = pane.turn_or_state_label();
        let state = if turn == duplex {
            turn
        } else {
            format!("{duplex}   {turn}")
        };
        format!("{carrier}   300 baud   {state}   {}", pane.elapsed_label())
    } else {
        format!("{carrier}  300  {}", pane.turn_or_state_label())
    }
}

impl Widget for &App {
    fn render(self, area: Rect, buf: &mut Buffer) {
        self.render_into(area, buf);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use modem_core::{Config, Duplex, Role};
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn pane(role: Role) -> Pane {
        Pane::new(Config {
            sample_rate: 8000,
            role,
            duplex: Duplex::HalfPingPong,
        })
    }

    /// A `Transport` that never touches a real device, for the two states
    /// this crate must tell apart - see `modem-audio`'s own doc: a real
    /// `CpalTransport` cannot be opened in CI.
    struct FakeTransport {
        acoustic: bool,
    }

    impl Transport for FakeTransport {
        fn run(
            &mut self,
            _ends: &mut [modem_core::session::Session],
        ) -> Result<modem_audio::transport::RunStats, modem_audio::transport::TransportError>
        {
            Ok(modem_audio::transport::RunStats::default())
        }
        fn sample_rate(&self) -> u32 {
            8000
        }
        fn is_acoustic(&self) -> bool {
            self.acoustic
        }
    }

    fn row_text(buf: &Buffer, y: u16) -> String {
        (0..buf.area.width)
            .map(|x| buf[(x, y)].symbol().to_string())
            .collect()
    }

    // --- Task 17: run_transport and step_wired -------------------------

    /// A `Transport` that mutates the sessions it is actually handed
    /// (answers the second one), so a test can tell "the real session
    /// round-tripped through `run_transport`" from "a fresh placeholder
    /// silently stood in for it".
    struct AnswerTransport;

    impl Transport for AnswerTransport {
        fn run(
            &mut self,
            ends: &mut [modem_core::session::Session],
        ) -> Result<modem_audio::transport::RunStats, modem_audio::transport::TransportError>
        {
            assert_eq!(
                ends.len(),
                2,
                "run_transport did not hand every pane's session through"
            );
            ends[1].answer();
            Ok(modem_audio::transport::RunStats::default())
        }
        fn sample_rate(&self) -> u32 {
            8000
        }
        fn is_acoustic(&self) -> bool {
            true
        }
    }

    /// Required behaviour: `run_transport` hands each pane's *real*
    /// session to the transport - not a copy, not a fresh placeholder -
    /// and hands the transport's own mutations back. Checked both ways:
    /// pane 0's own pre-existing `Dialling` state must have survived the
    /// round trip (a fresh placeholder would have reset it to `Idle`),
    /// and pane 1 must show the transport's own real mutation
    /// (`Answering`, from a session that started `Idle`), not something
    /// `run_transport` itself would ever produce.
    #[test]
    fn run_transport_hands_each_panes_real_session_to_the_transport_and_back() {
        let mut a = pane(Role::Originate);
        a.session_mut().dial("1");
        let b = pane(Role::Answer);
        let mut transport = AnswerTransport;
        let mut app = App::split(a, b, &transport, Theme::Amber);

        app.run_transport(&mut transport)
            .expect("run_transport must succeed");

        assert_eq!(
            app.panes()[0].turn_or_state_label(),
            "dialling",
            "pane 0's own session was replaced rather than round-tripped - its dialling state \
             was lost"
        );
        assert_eq!(
            app.panes()[1].turn_or_state_label(),
            "listening",
            "pane 1 does not show the transport's own real mutation - its session was not the \
             one the transport actually ran against"
        );
    }

    /// `step_wired` must feed the waterfall from the real audio the two
    /// panes' own sessions just exchanged, not merely avoid panicking -
    /// `fft_size_for(8000)` needs 512 samples for one FFT window, so two
    /// calls (256 samples each) must be enough to push at least one
    /// column.
    #[test]
    fn step_wired_feeds_the_waterfall_from_real_exchanged_audio() {
        let mut a = pane(Role::Originate);
        a.session_mut().dial("1");
        let mut b = pane(Role::Answer);
        b.session_mut().answer();
        let wired = modem_audio::WiredTransport::new(8000);
        let mut app = App::split(a, b, &wired, Theme::Amber);

        assert!(
            app.spectrum().is_empty(),
            "precondition failed: a fresh App must start with no waterfall history"
        );
        app.step_wired();
        app.step_wired();
        assert!(
            !app.spectrum().is_empty(),
            "step_wired did not feed any real audio to the waterfall"
        );
    }

    /// `step_wired` panics rather than silently doing nothing useful on a
    /// single-pane `App` - the wired demo always needs two ends to cross-
    /// wire, and the binary's own startup check is what actually prevents
    /// this combination from being reachable in practice (see the task
    /// report).
    #[test]
    #[should_panic(expected = "step_wired needs exactly two panes")]
    fn step_wired_panics_on_a_single_pane_app() {
        let wired = modem_audio::WiredTransport::new(8000);
        let mut app = App::single(pane(Role::Originate), &wired, Theme::Amber);
        app.step_wired();
    }

    // --- Required test: both layouts render at a range of sizes without
    // panicking or overflowing. ---
    #[test]
    fn both_layouts_render_at_a_wide_range_of_sizes_without_panicking() {
        let wired = modem_audio::WiredTransport::new(8000);
        for width in [0u16, 1, 2, 3, 10, 39, 40, 41, 79, 80, 81, 120, 200] {
            for height in [0u16, 1, 2, 3, 5, 10, 24, 60] {
                let single = App::single(pane(Role::Originate), &wired, Theme::Amber);
                let mut buf = Buffer::empty(Rect::new(0, 0, width, height));
                single.render_into(buf.area, &mut buf);

                let split = App::split(
                    pane(Role::Originate),
                    pane(Role::Answer),
                    &wired,
                    Theme::Amber,
                );
                let mut buf2 = Buffer::empty(Rect::new(0, 0, width, height));
                split.render_into(buf2.area, &mut buf2);
            }
        }
    }

    /// Same as above, but through a real `Terminal<TestBackend>` and
    /// `Widget::render_widget`, per the brief's own instruction to test
    /// against ratatui's `TestBackend` - the plain-`Buffer` sweep above
    /// covers far more sizes far more cheaply, but this proves the
    /// `Widget` impl used by the real event loop behaves the same way.
    #[test]
    fn renders_cleanly_through_a_real_terminal_and_test_backend() {
        let wired = modem_audio::WiredTransport::new(8000);
        let app = App::split(
            pane(Role::Originate),
            pane(Role::Answer),
            &wired,
            Theme::Amber,
        );
        let backend = TestBackend::new(100, 30);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                let area = f.area();
                f.render_widget(&app, area);
            })
            .unwrap();
    }

    // --- Required test: focus switching moves input to the other
    // session. ---
    #[test]
    fn f7_swaps_focus_and_keystrokes_follow_it() {
        let wired = modem_audio::WiredTransport::new(8000);
        let mut app = App::split(
            pane(Role::Originate),
            pane(Role::Answer),
            &wired,
            Theme::Amber,
        );
        assert_eq!(app.focus(), 0);
        app.handle_key(KeyEvent::new(
            KeyCode::Char('A'),
            crossterm::event::KeyModifiers::NONE,
        ));
        assert_eq!(app.panes()[0].composing(), "A");
        assert_eq!(app.panes()[1].composing(), "");

        app.handle_key(KeyEvent::new(
            KeyCode::F(7),
            crossterm::event::KeyModifiers::NONE,
        ));
        assert_eq!(app.focus(), 1);
        app.handle_key(KeyEvent::new(
            KeyCode::Char('T'),
            crossterm::event::KeyModifiers::NONE,
        ));
        assert_eq!(
            app.panes()[0].composing(),
            "A",
            "the first pane's buffer must be untouched by a keystroke sent after focus moved away from it"
        );
        assert_eq!(app.panes()[1].composing(), "T");
    }

    /// `F7` on a single-pane app must not panic even though there is
    /// nothing to swap to - `focus()` only ever indexes a real `panes`
    /// entry.
    #[test]
    fn f7_on_a_single_pane_app_does_nothing() {
        let wired = modem_audio::WiredTransport::new(8000);
        let mut app = App::single(pane(Role::Originate), &wired, Theme::Amber);
        app.handle_key(KeyEvent::new(
            KeyCode::F(7),
            crossterm::event::KeyModifiers::NONE,
        ));
        assert_eq!(app.focus(), 0);
    }

    /// A key `kind` other than `Press` (Windows reports `Release` too)
    /// must not double-feed a character.
    #[test]
    fn a_key_release_event_is_ignored() {
        let wired = modem_audio::WiredTransport::new(8000);
        let mut app = App::single(pane(Role::Originate), &wired, Theme::Amber);
        app.handle_key(KeyEvent::new_with_kind(
            KeyCode::Char('X'),
            crossterm::event::KeyModifiers::NONE,
            KeyEventKind::Release,
        ));
        assert_eq!(app.panes()[0].composing(), "");
    }

    // --- Required test: [DEMO MODE] appears when and only when the
    // transport is wired. ---
    #[test]
    fn demo_mode_is_read_from_the_transport_trait_method() {
        let wired = modem_audio::WiredTransport::new(8000);
        let app_wired = App::single(pane(Role::Originate), &wired, Theme::Amber);
        assert!(app_wired.demo_mode());

        let acoustic = FakeTransport { acoustic: true };
        let app_acoustic = App::single(pane(Role::Originate), &acoustic, Theme::Amber);
        assert!(!app_acoustic.demo_mode());
    }

    /// Position and content together: `[DEMO MODE]` must actually appear
    /// on the fkey bar row when wired, and must not appear anywhere in
    /// the whole rendered frame when acoustic - not merely "the app's
    /// `demo_mode()` flag is true", which the two unit tests above
    /// already establish independently of rendering at all.
    #[test]
    fn demo_mode_tag_appears_on_the_fkey_row_exactly_when_wired() {
        let wired = modem_audio::WiredTransport::new(8000);
        let app_wired = App::split(
            pane(Role::Originate),
            pane(Role::Answer),
            &wired,
            Theme::Amber,
        );
        let mut buf = Buffer::empty(Rect::new(0, 0, 100, 30));
        app_wired.render_into(buf.area, &mut buf);
        let fkey_row = row_text(&buf, 29);
        assert!(
            fkey_row.contains("[DEMO MODE]"),
            "wired transport must show [DEMO MODE] on the fkey row: {fkey_row:?}"
        );

        let acoustic = FakeTransport { acoustic: true };
        let app_acoustic = App::split(
            pane(Role::Originate),
            pane(Role::Answer),
            &acoustic,
            Theme::Amber,
        );
        let mut buf2 = Buffer::empty(Rect::new(0, 0, 100, 30));
        app_acoustic.render_into(buf2.area, &mut buf2);
        for y in 0..30 {
            let row = row_text(&buf2, y);
            assert!(
                !row.contains("DEMO MODE"),
                "acoustic transport must never show DEMO MODE anywhere, found it on row {y}: {row:?}"
            );
        }
    }

    // --- Mutation proof 2: derive demo_mode from the concrete type
    // rather than the trait method - see the task report for the actual
    // failing output, reproduced by temporarily changing App::single /
    // App::split to `demo_mode: false` (simulating "decided from which
    // struct, and got it wrong") and re-running the test above.

    // --- Rule 3: below ~80 columns split screen is unusable. ---
    #[test]
    fn split_below_eighty_columns_shows_a_message_not_a_broken_layout() {
        assert_eq!(
            decide_layout(RequestedLayout::Split, Rect::new(0, 0, 79, 24)),
            LayoutMode::TooNarrow
        );
        assert_eq!(
            decide_layout(RequestedLayout::Split, Rect::new(0, 0, 80, 24)),
            LayoutMode::Split
        );
    }

    #[test]
    fn too_narrow_message_actually_renders_instead_of_a_split_frame() {
        let wired = modem_audio::WiredTransport::new(8000);
        let app = App::split(
            pane(Role::Originate),
            pane(Role::Answer),
            &wired,
            Theme::Amber,
        );
        let mut buf = Buffer::empty(Rect::new(0, 0, 79, 24));
        app.render_into(buf.area, &mut buf);
        let top_row = row_text(&buf, 0);
        assert!(
            top_row.contains("80 columns"),
            "expected the too-narrow message on row 0, got {top_row:?}"
        );
        // None of the frame's own double-line glyphs should appear -
        // this is the message, not a squeezed frame.
        assert!(
            !top_row.contains('\u{2554}'),
            "must not draw the frame border when too narrow"
        );
    }

    // --- Rule 1: both panes render the same spectrum. ---
    #[test]
    fn split_screen_panes_render_identical_waterfalls() {
        let wired = modem_audio::WiredTransport::new(8000);
        let mut app = App::split(
            pane(Role::Originate),
            pane(Role::Answer),
            &wired,
            Theme::Amber,
        );
        // A non-uniform column, so a bug that swapped or zeroed one side
        // would actually change what is drawn - an all-zero or all-equal
        // spectrum could pass by coincidence.
        app.push_spectrum_column(vec![0.1, 0.9, 0.3, 1.0, 0.05, 0.6, 0.2, 0.8]);
        // 101, not 100: `frame::layout` gives the left column the extra
        // cell on an odd usable width, so a width that leaves both
        // columns *equal* is what makes a byte-for-byte comparison
        // meaningful here - an off-by-one column-width difference is a
        // legitimate, separate design choice (see `layout`'s own doc),
        // not the defect this test exists to catch.
        let mut buf = Buffer::empty(Rect::new(0, 0, 101, 24));
        app.render_into(buf.area, &mut buf);

        let layout = frame::layout(Rect::new(0, 0, 101, 23), 2, waterfall::DESIRED_ROWS);
        assert_eq!(
            layout.columns[0].width, layout.columns[1].width,
            "test precondition: both columns must be the same width"
        );
        let rows = layout.rows;
        let left = layout.columns[0];
        let right = layout.columns[1];
        let top = rows.waterfall_top.expect("waterfall must have rows here");
        for y in top..top + rows.waterfall_rows.min(waterfall::DESIRED_ROWS) {
            let left_bar: String = (left.left()..left.right())
                .map(|x| buf[(x, y)].symbol().to_string())
                .collect();
            let right_bar: String = (right.left()..right.right())
                .map(|x| buf[(x, y)].symbol().to_string())
                .collect();
            assert_eq!(
                left_bar, right_bar,
                "row {y}: the two panes drew different waterfall content from what must be one shared spectrum"
            );
        }
    }

    // --- Mutation proof 1: feed the two panes different spectra - see
    // the task report for the actual failing output, reproduced by
    // temporarily changing `render_frame` to pass `&Spectrum::silent(..)`
    // for one column and `&self.spectrum` for the other, and re-running
    // the test above.

    /// Split is what comes up with no flag. Asserted through
    /// `decide_layout` at a width that can hold it, so this pins the
    /// layout actually drawn rather than only the enum's discriminant -
    /// a `Default` pointing at `Split` while `decide_layout` ignored it
    /// would still pass an `assert_eq!(RequestedLayout::default(), ..)`.
    #[test]
    fn the_default_layout_is_split() {
        assert_eq!(RequestedLayout::default(), RequestedLayout::Split);
        assert_eq!(
            decide_layout(RequestedLayout::default(), Rect::new(0, 0, 100, 24)),
            LayoutMode::Split
        );
    }

    /// Both layouts have to work, not just the default one: single pane
    /// is the real product and split is the demo, and neither is allowed
    /// to become the one that only renders by accident.
    #[test]
    fn both_layouts_draw_their_own_chrome_at_their_own_widths() {
        let wired = modem_audio::WiredTransport::new(8000);

        let single = App::single(pane(Role::Originate), &wired, Theme::default());
        let mut buf = Buffer::empty(Rect::new(0, 0, 72, 24));
        single.render_into(buf.area, &mut buf);
        let title = row_text(&buf, 0);
        assert!(
            title.contains("modem") && title.contains("ORIGINATE"),
            "single pane lost its own title bar: {title:?}"
        );

        let split = App::split(
            pane(Role::Originate),
            pane(Role::Answer),
            &wired,
            Theme::default(),
        );
        let mut buf2 = Buffer::empty(Rect::new(0, 0, 100, 24));
        split.render_into(buf2.area, &mut buf2);
        let title2 = row_text(&buf2, 0);
        assert!(
            title2.contains("ORIGINATE") && title2.contains("ANSWER"),
            "split screen must title both ends on one row: {title2:?}"
        );
        assert!(
            row_text(&buf2, 23).contains("F7 swap focus"),
            "split screen must offer focus swapping"
        );
    }

    #[test]
    fn too_narrow_message_has_no_trailing_full_stop() {
        assert!(!"Split screen needs at least 80 columns (currently 40)".ends_with('.'));
    }

    #[test]
    fn no_chrome_string_in_this_module_uses_an_em_or_en_dash() {
        // The two bars are referenced, not re-typed: a copy here would
        // keep passing after the real strings changed, which is the
        // whole failure mode this crate keeps guarding against.
        let strings = [FKEY_BAR_SINGLE, FKEY_BAR_SPLIT, "[DEMO MODE]"];
        for s in strings {
            assert!(!s.contains('\u{2013}') && !s.contains('\u{2014}'), "{s:?}");
        }
    }

    /// The reduction has a number on it: one pane must fit in 72 columns
    /// so two windows sit side by side in 144. Asserted against the
    /// widest thing on each row rather than against a rendered frame,
    /// because a frame clips silently - it would render "fine" at 72 and
    /// simply lose the right-hand end of every line.
    #[test]
    fn the_single_pane_chrome_fits_in_seventy_two_columns() {
        const TARGET: u16 = 72;
        let wired = modem_audio::WiredTransport::new(8000);
        let app = App::single(pane(Role::Originate), &wired, Theme::default());
        let mut buf = Buffer::empty(Rect::new(0, 0, TARGET, 24));
        app.render_into(buf.area, &mut buf);

        // Rendered, not measured from the constants: the frame clips
        // silently, so the only way to know a row fits is to look at the
        // last thing on it after it has been drawn.
        let title = row_text(&buf, 0);
        assert!(
            title.contains("ORIGINATE \u{b7} 1270/1070") && title.ends_with('\u{2557}'),
            "title bar does not fit in {TARGET} columns: {title:?}"
        );

        let status = row_text(&buf, 1);
        assert!(
            status.contains("00:00:00"),
            "status line loses its elapsed field in {TARGET} columns: {status:?}"
        );

        let fkeys = row_text(&buf, 23);
        assert!(
            fkeys.contains("F10 hang up") && fkeys.contains("[DEMO MODE]"),
            "the fkey bar and the demo badge do not both fit in {TARGET} columns: {fkeys:?}"
        );
    }

    /// A received line longer than the pane is wide must appear in full
    /// on two rows, not lose its tail. Asserted on the two rows'
    /// contents joined back together, so a test cannot pass on a frame
    /// that merely *contains* the first half somewhere.
    #[test]
    fn a_line_wider_than_the_pane_wraps_instead_of_being_clipped() {
        let wired = modem_audio::WiredTransport::new(8000);
        let mut p = pane(Role::Originate);
        // Typed as a command line, which is how a long line gets into
        // the scrollback without a wire: the AT processor does not
        // recognise it, so the line itself lands in history followed by
        // ERROR. No test-only setter on `Pane` to go stale.
        let long =
            "the quick brown fox jumps over the lazy dog and keeps running well past the edge";
        p.type_line(long);
        let app = App::single(p, &wired, Theme::default());

        let mut buf = Buffer::empty(Rect::new(0, 0, 72, 24));
        app.render_into(buf.area, &mut buf);

        let rows: Vec<String> = (0..buf.area.height).map(|y| row_text(&buf, y)).collect();
        let head = rows
            .iter()
            .position(|r| r.contains("the quick brown fox"))
            .expect("the start of the long line is not on screen at all");
        // The two rows' contents, joined back together, must reconstruct
        // the line exactly - not merely contain a recognisable piece of
        // it. The wrap lands mid-word (a terminal wraps on columns, not
        // words), so any assertion looking for a whole phrase on one row
        // would be testing where the boundary happened to fall.
        let inner = |y: usize| rows[y].trim_matches('\u{2551}').trim_end().to_string();
        let rejoined = format!("{}{}", inner(head), inner(head + 1));
        assert_eq!(
            rejoined,
            long,
            "a long line must wrap onto the next row and lose nothing. \
             row {head}: {:?}, row {}: {:?}",
            rows[head],
            head + 1,
            rows[head + 1]
        );
    }

    #[test]
    fn wrap_splits_at_the_width_and_keeps_every_character() {
        let rows = wrap("abcdefghij", 4);
        assert_eq!(rows, vec!["abcd", "efgh", "ij"]);
        assert_eq!(rows.concat(), "abcdefghij");
    }

    /// A blank scrollback line must stay a row of its own - collapsing it
    /// would silently reflow the transcript.
    #[test]
    fn wrap_keeps_an_empty_line_as_one_row() {
        assert_eq!(wrap("", 10), vec![String::new()]);
    }

    /// Rule 2, stated accurately: the badge survives every width that can
    /// physically hold it, and below that nothing is drawn rather than a
    /// fragment. Codex's review was right that "always shown" was not
    /// literally true - 13 columns is the floor, and this pins it.
    #[test]
    fn below_the_badge_width_no_fragment_of_it_is_drawn() {
        let wired = modem_audio::WiredTransport::new(8000);
        for width in [1u16, 5, 12] {
            let app = App::single(pane(Role::Originate), &wired, Theme::default());
            let mut buf = Buffer::empty(Rect::new(0, 0, width, 24));
            app.render_into(buf.area, &mut buf);
            let row = row_text(&buf, 23);
            assert!(
                !row.contains('['),
                "a fragment of the badge was drawn at {width} columns: {row:?}"
            );
        }
    }

    /// Rule 2 again, at a width where both cannot fit: the badge is what
    /// survives. Before this, the bar was drawn first and the badge was
    /// clipped to a bare "[", which is both illegible and a silent loss
    /// of the only on-screen statement that the link is not acoustic.
    #[test]
    fn a_narrow_fkey_row_keeps_the_whole_badge_and_truncates_the_keys() {
        let wired = modem_audio::WiredTransport::new(8000);
        let app = App::single(pane(Role::Originate), &wired, Theme::default());
        let mut buf = Buffer::empty(Rect::new(0, 0, 40, 24));
        app.render_into(buf.area, &mut buf);

        let row = row_text(&buf, 23);
        assert!(
            row.contains("[DEMO MODE]"),
            "the whole badge must survive a narrow row, got {row:?}"
        );
        assert!(
            !row.contains("F10 hang up"),
            "the keys, not the badge, are what gets cut: {row:?}"
        );
    }

    // --- Task 18: the dialling directory overlay -----------------------

    /// Three entries with distinct, easily told-apart numbers - used by
    /// every overlay test below. Built fresh each call rather than
    /// shared, since `Directory` and `Entry` are cheap and a test that
    /// mutates its own `App`'s overlay must never share state with
    /// another test.
    fn three_entry_directory() -> Directory {
        Directory::parse(concat!(
            "Alice\t01111111111\n",
            "Bob\t02222222222\n",
            "Carol\t03333333333\n",
        ))
    }

    fn app_with_overlay(selected: usize) -> App {
        let wired = modem_audio::WiredTransport::new(8000);
        let mut app = App::single(pane(Role::Originate), &wired, Theme::default());
        app.directory_overlay = Some(DirectoryOverlay {
            directory: three_entry_directory(),
            path: PathBuf::from("/tmp/does-not-matter-for-this-test.tsv"),
            selected,
        });
        app
    }

    fn press(app: &mut App, code: KeyCode) {
        app.handle_key(KeyEvent::new(code, crossterm::event::KeyModifiers::NONE));
    }

    /// The overlay is drawn over the frame, so it must bring its own
    /// edges rather than writing on the frame's. The first version wrote
    /// at the frame's own coordinates, which replaced the double-line
    /// rails with text and rendered an empty directory as
    /// `-no entries - looked in ...-`. Asserted by position: every row
    /// the overlay covers must still start and end with the frame's own
    /// rail, and the overlay must have drawn its own corners.
    #[test]
    fn the_overlay_draws_its_own_border_and_never_over_the_frames() {
        for directory in [three_entry_directory(), Directory::parse("")] {
            let wired = modem_audio::WiredTransport::new(8000);
            let mut app = App::single(pane(Role::Originate), &wired, Theme::default());
            app.directory_overlay = Some(DirectoryOverlay {
                directory,
                path: PathBuf::from("/tmp/a-very-long-path-that-would-overflow-the-frame.tsv"),
                selected: 0,
            });

            let mut buf = Buffer::empty(Rect::new(0, 0, 72, 24));
            app.render_into(buf.area, &mut buf);

            // Rows 1..22 are inside the frame's own box; row 0 is its top
            // rule and row 22 its bottom, with the fkey bar on row 23.
            // The frame's own left and right edge characters: a plain
            // rail on a content row, a tee on one of its divider rows.
            const EDGES: [char; 3] = ['\u{2551}', '\u{2560}', '\u{2563}'];
            for y in 1..22u16 {
                let row = row_text(&buf, y);
                let first = row.chars().next().unwrap();
                let last = row.chars().last().unwrap();
                assert!(
                    EDGES.contains(&first) && EDGES.contains(&last),
                    "the overlay wrote over the frame's own edge on row {y}: {row:?}"
                );
            }

            let all: String = (0..buf.area.height)
                .map(|y| row_text(&buf, y))
                .collect::<Vec<_>>()
                .join("");
            for corner in ['\u{250C}', '\u{2510}', '\u{2514}', '\u{2518}'] {
                assert!(
                    all.contains(corner),
                    "the overlay did not draw its own {corner:?} corner"
                );
            }

            // Nothing underneath shows through. The panel has to start in
            // the very first column inside the frame's rail: insetting it
            // by even one cell leaves a strip of the waterfall's own
            // frequency labels visible down the panel's left edge.
            const PANEL_EDGE: [char; 3] = ['\u{2502}', '\u{250C}', '\u{2514}'];
            for y in 1..22u16 {
                let row = row_text(&buf, y);
                let inside = row.chars().nth(1).unwrap();
                assert!(
                    PANEL_EDGE.contains(&inside),
                    "the pane underneath shows through beside the overlay on row {y}: {row:?}"
                );
            }
        }
    }

    // Required test: wrapping over a full cycle, not a single step. The
    // brief's own warning is that a `next` which always returned 0 would
    // pass a test that only checks "after some downs, are we back at
    // 0?" - starting selection is already 0, so that check alone cannot
    // tell a genuine wrap from a `next` that never moves at all. Instead
    // this records the selection after each of four consecutive Downs
    // and checks the whole sequence: [1, 2, 0, 1]. The `0` at step three
    // proves the wrap happened; the `1` at step four (not another 0)
    // proves it did not get stuck there - a mutation that clamped at the
    // end (proof 1) fails at step three (2 again, not 0), and a `next`
    // that always returns 0 fails at step one already (0, not 1).
    #[test]
    fn down_wraps_through_a_full_cycle_and_keeps_advancing_correctly_past_the_wrap() {
        let mut app = app_with_overlay(0);
        let mut seen = Vec::new();
        for _ in 0..4 {
            press(&mut app, KeyCode::Down);
            seen.push(app.directory_overlay.as_ref().unwrap().selected);
        }
        assert_eq!(
            seen,
            vec![1, 2, 0, 1],
            "down must wrap at the end of a three-entry directory and keep advancing \
             correctly afterwards, got {seen:?}"
        );
    }

    #[test]
    fn up_from_the_first_entry_wraps_to_the_last() {
        let mut app = app_with_overlay(0);
        press(&mut app, KeyCode::Up);
        assert_eq!(
            app.directory_overlay.as_ref().unwrap().selected,
            2,
            "up from entry 0 of a three-entry directory must wrap to entry 2"
        );
    }

    // --- Mutation proof 1 (see the task report for the actual run):
    // clamp instead of wrap - change `next_selection`/`prev_selection` to
    // `(selected + 1).min(len - 1)` / `selected.saturating_sub(1)`. Both
    // tests above must fail.

    /// Required test: Enter dials the *highlighted* entry, not the
    /// first. Deliberately never asserts on `SessionState` - reaching
    /// `Dialling` proves nothing about which number was dialled, only
    /// that dialling of some kind happened. The actual proof is the
    /// literal `ATDT<digits>` line the third entry's own number produces,
    /// found in the pane's real scrollback.
    #[test]
    fn enter_dials_the_highlighted_entry_not_the_first() {
        let mut app = app_with_overlay(2); // Carol, the third entry
        press(&mut app, KeyCode::Enter);

        assert!(
            app.directory_overlay.is_none(),
            "Enter must close the overlay"
        );
        assert_eq!(
            app.panes()[0].history().first(),
            Some(&"ATDT03333333333".to_string()),
            "Enter must dial the highlighted (third) entry's own number, not the first \
             entry's - got history {:?}",
            app.panes()[0].history()
        );
    }

    // --- Mutation proof 2 (see the task report for the actual run): dial
    // `entries[0]` unconditionally instead of `entries[overlay.selected]`
    // - the test above must fail (it would dial Alice's number instead
    // of Carol's).

    /// Required test: Esc closes without dialling - no `ATDT` anywhere in
    /// the scrollback afterwards.
    #[test]
    fn esc_closes_without_dialling() {
        let mut app = app_with_overlay(1);
        press(&mut app, KeyCode::Esc);

        assert!(
            app.directory_overlay.is_none(),
            "Esc must close the overlay"
        );
        assert!(
            !app.panes()[0].history().iter().any(|l| l.contains("ATDT")),
            "Esc must never dial, got history {:?}",
            app.panes()[0].history()
        );
    }

    /// A second `F2` also closes without dialling - the forgiving
    /// counterpart to Esc, not required by the brief but cheap to offer
    /// and cheap to pin here.
    #[test]
    fn a_second_f2_closes_the_overlay_without_dialling() {
        let mut app = app_with_overlay(1);
        press(&mut app, KeyCode::F(2));
        assert!(app.directory_overlay.is_none());
        assert!(!app.panes()[0].history().iter().any(|l| l.contains("ATDT")));
    }

    /// Required test: an empty directory renders a message saying where
    /// it looked, and does not panic.
    #[test]
    fn empty_directory_overlay_says_where_it_looked_and_does_not_panic() {
        let wired = modem_audio::WiredTransport::new(8000);
        let mut app = App::single(pane(Role::Originate), &wired, Theme::default());
        app.directory_overlay = Some(DirectoryOverlay {
            directory: Directory::parse(""),
            path: PathBuf::from("/home/example/.config/modem/directory.tsv"),
            selected: 0,
        });

        let mut buf = Buffer::empty(Rect::new(0, 0, 72, 24));
        app.render_into(buf.area, &mut buf);

        let rows: Vec<String> = (0..buf.area.height).map(|y| row_text(&buf, y)).collect();
        assert!(
            rows.iter()
                .any(|r| r.contains("/home/example/.config/modem/directory.tsv")),
            "an empty directory must say where it looked: {rows:?}"
        );
    }

    /// Required test: the overlay renders at a range of sizes without
    /// panicking, including sizes smaller than the overlay's own content
    /// (three entries plus a header needs at least four rows, and the
    /// longest line here is well over ten columns wide).
    #[test]
    fn directory_overlay_renders_at_a_range_of_sizes_without_panicking() {
        let wired = modem_audio::WiredTransport::new(8000);
        for width in [0u16, 1, 2, 5, 10, 30, 72, 100] {
            for height in [0u16, 1, 2, 3, 10, 24] {
                let mut app = App::single(pane(Role::Originate), &wired, Theme::default());
                app.directory_overlay = Some(DirectoryOverlay {
                    directory: three_entry_directory(),
                    path: PathBuf::from("/tmp/does-not-matter-for-this-test.tsv"),
                    selected: 1,
                });
                let mut buf = Buffer::empty(Rect::new(0, 0, width, height));
                app.render_into(buf.area, &mut buf);
            }
        }
    }

    /// The selected entry's own line must actually look different from
    /// the others once rendered, not merely carry a different `selected`
    /// index internally that nothing on screen reflects.
    #[test]
    fn the_highlighted_entry_is_visibly_marked_in_the_rendered_overlay() {
        let app = app_with_overlay(1); // Bob
        let mut buf = Buffer::empty(Rect::new(0, 0, 72, 24));
        app.render_into(buf.area, &mut buf);

        let rows: Vec<String> = (0..buf.area.height).map(|y| row_text(&buf, y)).collect();
        assert!(
            rows.iter().any(|r| r.contains("> Bob")),
            "the selected entry (Bob) must carry a visible marker immediately before its \
             name, got {rows:?}"
        );
        assert!(
            !rows
                .iter()
                .any(|r| r.contains("> Alice") || r.contains("> Carol")),
            "only the selected entry may carry the marker, got {rows:?}"
        );
    }

    #[test]
    fn the_fkey_bar_advertises_f2_directory_before_it_is_ever_opened() {
        let wired = modem_audio::WiredTransport::new(8000);
        let app = App::single(pane(Role::Originate), &wired, Theme::default());
        assert!(app.directory_overlay.is_none());
        let mut buf = Buffer::empty(Rect::new(0, 0, 72, 24));
        app.render_into(buf.area, &mut buf);
        assert!(row_text(&buf, 23).contains("F2 directory"));
    }
}
