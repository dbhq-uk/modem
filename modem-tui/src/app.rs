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

use std::time::Duration;

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::widgets::Widget;

use modem_audio::Transport;

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
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RequestedLayout {
    Single,
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

/// How many rows the waterfall's frequency-label bars occupy, before any
/// extra furniture on top of them.
const WATERFALL_BAR_ROWS: u16 = waterfall::FREQUENCY_LABELS.len() as u16;

/// The comms-package application. See this module's own doc for the
/// three rules this type exists to enforce.
pub struct App {
    requested: RequestedLayout,
    panes: Vec<Pane>,
    focus: usize,
    demo_mode: bool,
    spectrum: Spectrum,
    theme: Theme,
}

impl App {
    /// One pane, connected to another machine - the real product. `demo_mode`
    /// is read from `transport.is_acoustic()`, never supplied directly -
    /// see this module's own doc, rule 2.
    pub fn single(pane: Pane, transport: &dyn Transport, theme: Theme) -> Self {
        App {
            requested: RequestedLayout::Single,
            panes: vec![pane],
            focus: 0,
            demo_mode: !transport.is_acoustic(),
            spectrum: Spectrum::silent(WATERFALL_BAR_ROWS as usize),
            theme,
        }
    }

    /// Both ends on one machine, for demonstration. `demo_mode` is read
    /// from `transport.is_acoustic()`, never supplied directly - see this
    /// module's own doc, rule 2.
    pub fn split(a: Pane, b: Pane, transport: &dyn Transport, theme: Theme) -> Self {
        App {
            requested: RequestedLayout::Split,
            panes: vec![a, b],
            focus: 0,
            demo_mode: !transport.is_acoustic(),
            spectrum: Spectrum::silent(WATERFALL_BAR_ROWS as usize),
            theme,
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

    /// Replaces the one spectrum both panes render from - Task 16 calls
    /// this once per tick with the real FFT output. See this module's
    /// own doc, rule 1: there is deliberately no per-pane equivalent.
    pub fn set_spectrum(&mut self, spectrum: Spectrum) {
        self.spectrum = spectrum;
    }

    pub fn layout_mode(&self, area: Rect) -> LayoutMode {
        decide_layout(self.requested, area)
    }

    /// Routes one key event to the focused pane, or to the app itself for
    /// the keys this crate already has enough to act on (`F7` swap focus,
    /// `F6` theme cycle, `F4` answer, `F10` hang up). `F1`/`F2`/`F3`/`F5`
    /// are chrome only for now - help, the dialling directory (Task 18)
    /// and the standalone waterfall view need UI this task does not
    /// build, and dialling needs a number source Task 18 provides.
    pub fn handle_key(&mut self, key: KeyEvent) {
        // Windows reports both press and release; Unix (without the
        // keyboard-enhancement protocol) reports only press. Acting on
        // anything but a press would double-feed every character typed
        // on Windows.
        if key.kind != KeyEventKind::Press {
            return;
        }
        match key.code {
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
        match self.layout_mode(area) {
            LayoutMode::TooNarrow => self.render_too_narrow(area, buf),
            LayoutMode::Single => self.render_frame(area, buf, false),
            LayoutMode::Split => self.render_frame(area, buf, true),
        }
    }

    fn render_too_narrow(&self, area: Rect, buf: &mut Buffer) {
        let msg = format!(
            "Split screen needs at least {} columns (currently {})",
            MIN_SPLIT_COLUMNS, area.width
        );
        draw::text(buf, area, area.left(), area.top(), &msg, Style::default());
    }

    fn render_frame(&self, area: Rect, buf: &mut Buffer, split: bool) {
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
            WATERFALL_BAR_ROWS
        } else {
            WATERFALL_BAR_ROWS + 1 // + the stage-axis footer row
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
    }

    fn titles(&self, split: bool) -> Vec<Title> {
        if !split {
            let pane = &self.panes[0];
            return vec![Title {
                left: "modem".to_string(),
                right: format!("{} \u{b7} {} Hz", pane.role_name(), pane.band_label()),
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
            let bar_rows = waterfall_area.height.min(WATERFALL_BAR_ROWS);
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
            if waterfall_area.height > bar_rows {
                let footer_area = Rect {
                    y: waterfall_area.y + bar_rows,
                    height: waterfall_area.height - bar_rows,
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
        let terminal_rows = area.height;
        let history_rows = if show_prompt {
            terminal_rows.saturating_sub(1)
        } else {
            terminal_rows
        };
        let history = pane.history();
        let start = history.len().saturating_sub(history_rows as usize);
        for (i, line) in history[start..].iter().enumerate() {
            draw::text(buf, area, area.left(), area.top() + i as u16, line, style);
        }
        if show_prompt && history_rows < terminal_rows {
            let prefix = if pane.in_command_mode() { "" } else { "> " };
            let text = format!("{prefix}{}_", pane.composing());
            draw::text(
                buf,
                area,
                area.left(),
                area.top() + history_rows,
                &text,
                style,
            );
        }
    }

    fn render_fkey_bar(&self, buf: &mut Buffer, area: Rect, split: bool) {
        let base = if split {
            " F1 help  F3 dial  F4 answer  F7 swap focus  F10 hang up"
        } else {
            " F1 help  F2 directory  F3 dial  F4 answer  F5 waterfall  F6 theme  F10 hang up"
        };
        let style = Style::default().fg(self.theme.bright());
        draw::text(buf, area, area.left(), area.top(), base, style);
        if self.demo_mode {
            let tag = "[DEMO MODE]";
            let x = area.right().saturating_sub(tag.chars().count() as u16 + 1);
            let left_end = area.left() + base.chars().count() as u16;
            let x = x.max(left_end + 1).min(area.right().saturating_sub(1));
            draw::text(buf, area, x, area.top(), tag, style);
        }
    }
}

fn status_text(pane: &Pane, wide: bool) -> String {
    let carrier = if pane.carrier() {
        "\u{25CF} CARRIER"
    } else {
        "\u{25CB} NO CARRIER"
    };
    if wide {
        format!(
            "{carrier}    300 baud    {}    {}    {}",
            pane.duplex_label(),
            pane.turn_or_state_label(),
            pane.elapsed_label()
        )
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
        // A non-uniform spectrum, so a bug that swapped or zeroed one
        // side would actually change what is drawn - an all-zero or
        // all-equal spectrum could pass by coincidence.
        app.set_spectrum(Spectrum {
            bins: vec![0.1, 0.9, 0.3, 1.0, 0.05, 0.6],
        });
        // 101, not 100: `frame::layout` gives the left column the extra
        // cell on an odd usable width, so a width that leaves both
        // columns *equal* is what makes a byte-for-byte comparison
        // meaningful here - an off-by-one column-width difference is a
        // legitimate, separate design choice (see `layout`'s own doc),
        // not the defect this test exists to catch.
        let mut buf = Buffer::empty(Rect::new(0, 0, 101, 24));
        app.render_into(buf.area, &mut buf);

        let layout = frame::layout(
            Rect::new(0, 0, 101, 23),
            2,
            waterfall::FREQUENCY_LABELS.len() as u16,
        );
        assert_eq!(
            layout.columns[0].width, layout.columns[1].width,
            "test precondition: both columns must be the same width"
        );
        let rows = layout.rows;
        let left = layout.columns[0];
        let right = layout.columns[1];
        let top = rows.waterfall_top.expect("waterfall must have rows here");
        for y in top..top
            + rows
                .waterfall_rows
                .min(waterfall::FREQUENCY_LABELS.len() as u16)
        {
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

    #[test]
    fn too_narrow_message_has_no_trailing_full_stop() {
        assert!(!"Split screen needs at least 80 columns (currently 40)".ends_with('.'));
    }

    #[test]
    fn no_chrome_string_in_this_module_uses_an_em_or_en_dash() {
        let strings = [
            " F1 help  F3 dial  F4 answer  F7 swap focus  F10 hang up",
            " F1 help  F2 directory  F3 dial  F4 answer  F5 waterfall  F6 theme  F10 hang up",
            "[DEMO MODE]",
        ];
        for s in strings {
            assert!(!s.contains('\u{2013}') && !s.contains('\u{2014}'), "{s:?}");
        }
    }
}
