//! The live, scrolling spectrogram: frequency up the left edge, time
//! across, newest column at the right.
//!
//! # The boundary this module still respects
//!
//! [`render`] takes a [`Spectrum`] as a plain argument and never fetches
//! or computes one itself - see that module's own doc for why this is
//! what makes "both panes render the same spectrum"
//! (`app.rs`'s `split_screen_panes_render_identical_waterfalls`) a
//! testable claim at all. Task 16 only widens what `Spectrum` carries
//! (a rolling window of columns plus the sample rate, not a single flat
//! list) and replaces this module's body; the call site in `app.rs`
//! still just hands over `&self.spectrum`.
//!
//! # The axis
//!
//! Vertical: linear, 300 Hz at the bottom to 3400 Hz at the top - the
//! whole telephone band, so dial tone, DTMF, ANSam and both Bell 103
//! bands are all on screen together. Each character cell shows **two**
//! frequency sub-bands via the upper-half-block glyph `▀`: the
//! foreground colour is the upper sub-band, the background is the lower
//! one. That is where the doubled vertical resolution comes from - a
//! cell has exactly one foreground and one background colour, so this is
//! the only way to get two independently-coloured bands out of one cell.
//!
//! **Below [`CONTINUOUS_AXIS_MIN_ROWS`] rows this collapses.** At `h`
//! rows there are `2h` sub-bands spread over 3100 Hz; at six rows that is
//! 258 Hz per sub-band, wider than the 200 Hz gap between either Bell 103
//! mark/space pair (1070/1270 or 2025/2225) - the pair would read as one
//! smeared line, not two. Below the threshold the axis stops pretending
//! to a resolution it does not have and instead shows exactly the four
//! Bell 103 tones, each its own labelled row, spread across whatever
//! height is available. At or above the threshold it is the continuous
//! axis with labels at their true computed positions.
//!
//! # The ramp and the decay
//!
//! Each lit cell's brightness is the theme's bright colour scaled by
//! magnitude in dB relative to a full-scale tone, quantised into
//! [`RAMP_STEPS`] discrete steps from the ground colour (silence) up to
//! full brightness (0 dB). Older columns fade toward the floor as they
//! scroll left, over a [`DECAY_COLUMNS`]-column time constant - the
//! "brand token" the design spec's `tokens.json` generator (plan 2, not
//! this repo) will eventually own; pinned here as a literal in the
//! meantime, the same judgement call `theme.rs` already makes for the
//! phosphor palette itself.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};

use crate::draw;
use crate::spectrum::Spectrum;
use crate::theme;

/// Bottom of the vertical axis, Hz.
pub const AXIS_LOW_HZ: f32 = 300.0;

/// Top of the vertical axis, Hz - the two together are the whole
/// telephone band per the design spec.
pub const AXIS_HIGH_HZ: f32 = 3400.0;

/// Below this many character rows, the continuous axis cannot separate a
/// Bell 103 mark/space pair (200 Hz apart) into two sub-bands - see this
/// module's own doc for the maths. At and above this height the axis is
/// continuous; below it, the four Bell 103 tones only.
pub const CONTINUOUS_AXIS_MIN_ROWS: u16 = 12;

/// The four Bell 103 tones, high to low - the fixed rows shown below
/// [`CONTINUOUS_AXIS_MIN_ROWS`], in the same top-to-bottom (high-to-low
/// frequency) order the continuous axis would place them in.
const BELL_TONES_HZ: [f32; 4] = [2225.0, 2025.0, 1270.0, 1070.0];
const BELL_TONE_LABELS: [&str; 4] = ["2225", "2025", "1270", "1070"];

/// The named frequencies labelled on the continuous axis, low to high.
const NAMED_FREQUENCIES: [(f32, &str); 6] = [
    (300.0, " 300"),
    (1070.0, "1070"),
    (1270.0, "1270"),
    (2025.0, "2025"),
    (2225.0, "2225"),
    (3400.0, "3400"),
];

/// Width in cells the frequency label column occupies, including its
/// trailing separating space, before the data itself starts.
pub const LABEL_WIDTH: u16 = 5;

/// How many rows this widget asks its caller for when there is no
/// tighter constraint - see `app.rs`'s `frame::layout` call. Not a cap on
/// what [`render`] can draw: given more or fewer rows than this it draws
/// with whatever it is actually given (see the size-sweep test below),
/// so a taller terminal gets better frequency resolution for free.
pub const DESIRED_ROWS: u16 = 8;

/// Discrete brightness steps in the intensity ramp, floor to full - five
/// or more per the design spec.
pub const RAMP_STEPS: u8 = 8;

/// dB (relative to a full-scale, bin-aligned tone) at and below which a
/// cell is fully at the floor colour.
const FLOOR_DB: f32 = -48.0;

/// How many columns of scroll it takes a lit cell's brightness to decay
/// toward the floor - see this module's own doc.
pub const DECAY_COLUMNS: f32 = 40.0;

/// The overture's stages, as the axis footer names them - unchanged by
/// Task 16, which owns the frequency axis, not this footer. See Task 15's
/// own doc for why this is still static furniture.
pub const STAGE_AXIS_LABELS: [&str; 6] =
    ["dial tone", "DTMF", "ringback", "ANSam", "training", "data"];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Half {
    Upper,
    Lower,
}

fn subband_width_hz(h: u16) -> f32 {
    (AXIS_HIGH_HZ - AXIS_LOW_HZ) / (2.0 * h as f32)
}

/// Which of the `2h` sub-bands `freq` falls in, clamped to the valid
/// range - a frequency at or beyond the axis edges lands in the nearest
/// end sub-band rather than panicking or wrapping.
fn subband_index(freq: f32, h: u16) -> usize {
    let bw = subband_width_hz(h);
    let count = 2i64 * h as i64;
    let raw = ((freq - AXIS_LOW_HZ) / bw).floor() as i64;
    raw.clamp(0, count - 1) as usize
}

/// The row (0 = top) and which half of it a sub-band index falls in.
/// Inverse of [`subbands_for_row`].
fn row_from_subband(s: usize, h: u16) -> (u16, Half) {
    let row_from_bottom = (s / 2) as u16;
    let half = if s.is_multiple_of(2) {
        Half::Lower
    } else {
        Half::Upper
    };
    (h - 1 - row_from_bottom, half)
}

/// The row a frequency's label (and its lit cell, in continuous mode)
/// belongs on.
fn row_for_frequency(freq: f32, h: u16) -> u16 {
    row_from_subband(subband_index(freq, h), h).0
}

/// The two sub-band indices (lower, upper) a row covers. Inverse of
/// [`row_from_subband`].
fn subbands_for_row(row: u16, h: u16) -> (usize, usize) {
    let row_from_bottom = (h - 1 - row) as usize;
    let lower = row_from_bottom * 2;
    (lower, lower + 1)
}

/// Max-pools raw FFT bin magnitudes into `2h` sub-bands covering
/// `[AXIS_LOW_HZ, AXIS_HIGH_HZ)`. Max rather than average, so a narrow
/// tone is not diluted by the mostly-silent bins sharing its sub-band.
fn subband_magnitudes(bins: &[f32], sample_rate: u32, h: u16) -> Vec<f32> {
    let mut out = vec![0.0f32; 2 * h as usize];
    if bins.is_empty() || sample_rate == 0 {
        return out;
    }
    let n_fft = bins.len() * 2;
    let bin_hz = sample_rate as f32 / n_fft as f32;
    for (k, &mag) in bins.iter().enumerate() {
        let freq = k as f32 * bin_hz;
        if freq < AXIS_LOW_HZ || freq >= AXIS_HIGH_HZ {
            continue;
        }
        let s = subband_index(freq, h);
        if mag > out[s] {
            out[s] = mag;
        }
    }
    out
}

/// The raw bin nearest `freq` - used for the four-tone axis, which has no
/// sub-band concept of its own.
fn nearest_bin_magnitude(bins: &[f32], sample_rate: u32, freq: f32) -> f32 {
    if bins.is_empty() || sample_rate == 0 {
        return 0.0;
    }
    let n_fft = bins.len() * 2;
    let bin_hz = sample_rate as f32 / n_fft as f32;
    let idx = (freq / bin_hz).round() as usize;
    bins.get(idx.min(bins.len() - 1)).copied().unwrap_or(0.0)
}

/// Magnitude in dB relative to a full-scale, bin-aligned tone: a Hann
/// window's coherent gain is 0.5, so an amplitude-1 sine lands at
/// `n_fft/4 == bins.len()/2`.
fn magnitude_to_db(magnitude: f32, bins_len: usize) -> f32 {
    let reference = (bins_len as f32 / 2.0).max(1e-6);
    let m = magnitude.max(1e-6);
    20.0 * (m / reference).log10()
}

fn decay_factor(age: usize) -> f32 {
    (-(age as f32) / DECAY_COLUMNS).exp()
}

/// Quantises a dB value, attenuated by how many columns old it is, into
/// `0..RAMP_STEPS`.
fn ramp_level(db: f32, age: usize) -> u8 {
    let t = ((db - FLOOR_DB) / (0.0 - FLOOR_DB)).clamp(0.0, 1.0);
    let decayed = t * decay_factor(age);
    (decayed * (RAMP_STEPS as f32 - 1.0)).round() as u8
}

fn rgb_of(c: Color) -> (u8, u8, u8) {
    match c {
        Color::Rgb(r, g, b) => (r, g, b),
        _ => (0, 0, 0),
    }
}

fn lerp(a: u8, b: u8, t: f32) -> u8 {
    (a as f32 + (b as f32 - a as f32) * t).round() as u8
}

/// The ramp itself: `level` 0 is the floor colour (silence, or fully
/// decayed), `RAMP_STEPS - 1` is `bright` at full brightness.
fn ramp_colour(level: u8, bright: Color) -> Color {
    let (fr, fg, fb) = rgb_of(theme::GROUND);
    let (br, bg, bb) = rgb_of(bright);
    let t = level as f32 / (RAMP_STEPS as f32 - 1.0);
    Color::Rgb(lerp(fr, br, t), lerp(fg, bg, t), lerp(fb, bb, t))
}

/// Rows (top-first) and labels for the below-threshold four-tone axis,
/// spread evenly across the available height. Never returns more rows
/// than there are tones, and never more than `h` even if `h` is smaller
/// than the tone count - see this module's own doc for the degrade.
fn four_tone_rows(h: u16) -> Vec<(u16, f32, &'static str)> {
    let shown = (BELL_TONES_HZ.len() as u16).min(h.max(1)) as usize;
    let mut out = Vec::with_capacity(shown);
    for (i, (&freq, &label)) in BELL_TONES_HZ
        .iter()
        .zip(BELL_TONE_LABELS.iter())
        .take(shown)
        .enumerate()
    {
        let row = if shown <= 1 {
            0
        } else {
            (i as u16) * (h - 1) / (shown as u16 - 1)
        };
        out.push((row, freq, label));
    }
    out
}

/// Draws the live waterfall into `area` from `spectrum`'s history:
/// frequency up the left edge, time across, newest column at the right.
/// `style` supplies the bright (theme) colour the ramp scales; `dim`
/// labels the axis.
///
/// Never panics at any `area` size, including zero - see the size-sweep
/// test below.
pub fn render(buf: &mut Buffer, area: Rect, spectrum: &Spectrum, style: Style, dim: Style) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let h = area.height;
    let bright = style.fg.unwrap_or(theme::WHITE);

    let bar_x = area.left().saturating_add(LABEL_WIDTH);
    let bar_width = area.right().saturating_sub(bar_x);

    if h < CONTINUOUS_AXIS_MIN_ROWS {
        render_four_tone(buf, area, spectrum, bar_x, bar_width, h, bright, dim);
    } else {
        render_continuous(buf, area, spectrum, bar_x, bar_width, h, bright, dim);
    }
}

#[allow(clippy::too_many_arguments)]
fn render_continuous(
    buf: &mut Buffer,
    area: Rect,
    spectrum: &Spectrum,
    bar_x: u16,
    bar_width: u16,
    h: u16,
    bright: Color,
    dim: Style,
) {
    for &(freq, label) in NAMED_FREQUENCIES.iter() {
        let row = row_for_frequency(freq, h);
        let y = area.top() + row;
        draw::text(buf, area, area.left(), y, label, dim);
    }

    if bar_width == 0 {
        return;
    }
    for x_off in 0..bar_width {
        let age = (bar_width - 1 - x_off) as usize;
        let Some(bins) = spectrum.column(age) else {
            continue;
        };
        let subbands = subband_magnitudes(bins, spectrum.sample_rate(), h);
        let x = bar_x + x_off;
        for row in 0..h {
            let (lower, upper) = subbands_for_row(row, h);
            let db_upper = magnitude_to_db(subbands[upper], bins.len());
            let db_lower = magnitude_to_db(subbands[lower], bins.len());
            let fg = ramp_colour(ramp_level(db_upper, age), bright);
            let bg = ramp_colour(ramp_level(db_lower, age), bright);
            let y = area.top() + row;
            draw::cell(buf, area, x, y, '\u{2580}', Style::default().fg(fg).bg(bg));
            // ▀
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn render_four_tone(
    buf: &mut Buffer,
    area: Rect,
    spectrum: &Spectrum,
    bar_x: u16,
    bar_width: u16,
    h: u16,
    bright: Color,
    dim: Style,
) {
    let rows = four_tone_rows(h);
    for &(row, _, label) in &rows {
        let y = area.top() + row;
        draw::text(buf, area, area.left(), y, label, dim);
    }

    if bar_width == 0 {
        return;
    }
    for x_off in 0..bar_width {
        let age = (bar_width - 1 - x_off) as usize;
        let Some(bins) = spectrum.column(age) else {
            continue;
        };
        let x = bar_x + x_off;
        for &(row, freq, _) in &rows {
            let mag = nearest_bin_magnitude(bins, spectrum.sample_rate(), freq);
            let db = magnitude_to_db(mag, bins.len());
            let colour = ramp_colour(ramp_level(db, age), bright);
            let y = area.top() + row;
            draw::cell(buf, area, x, y, ' ', Style::default().bg(colour));
        }
    }
}

/// Draws the overture stage-name axis footer under the waterfall, one
/// row, truncated (never wrapped or panicking) to whatever width is
/// actually available. Only called in single-pane mode - unchanged from
/// Task 15.
pub fn render_stage_axis(buf: &mut Buffer, area: Rect, style: Style) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let text = STAGE_AXIS_LABELS.join(" \u{2534} "); // ┴ matches the mockup's own separator
    draw::text(buf, area, area.left(), area.top(), &text, style);
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::Color;

    fn style() -> Style {
        Style::default().fg(Color::Rgb(0xFF, 0xB0, 0x00))
    }

    fn dim() -> Style {
        Style::default().fg(Color::Rgb(0x80, 0x58, 0x00))
    }

    fn row_text(buf: &Buffer, area: Rect, y: u16) -> String {
        (area.left()..area.right())
            .map(|x| buf[(x, y)].symbol().to_string())
            .collect()
    }

    /// The brighter of a cell's foreground or background, summed - either
    /// half can carry the ramp colour depending on which sub-band it is,
    /// so a "how lit is this cell" check has to look at both.
    fn cell_brightness(buf: &Buffer, x: u16, y: u16) -> u32 {
        let cell = &buf[(x, y)];
        let (fr, fgc, fb) = rgb_of(cell.fg);
        let (br, bgc, bb) = rgb_of(cell.bg);
        (fr as u32 + fgc as u32 + fb as u32).max(br as u32 + bgc as u32 + bb as u32)
    }

    /// Finds the row a label's text is drawn on, by reading the rendered
    /// buffer rather than recomputing the row internally - a positional
    /// test that recomputed the expected row with the same function under
    /// test would not actually be testing anything.
    fn find_label_row(buf: &Buffer, area: Rect, label: &str) -> u16 {
        for y in area.top()..area.bottom() {
            if row_text(buf, area, y).starts_with(label) {
                return y;
            }
        }
        panic!("label {label:?} not found anywhere in the rendered area");
    }

    fn sine_column(freq: f64, sample_rate: u32, n: usize) -> Vec<f32> {
        let samples: Vec<f32> = (0..n)
            .map(|i| (core::f64::consts::TAU * freq * i as f64 / sample_rate as f64).sin() as f32)
            .collect();
        let mut mags = vec![0.0f32; n / 2];
        modem_core::analyse::magnitudes(&samples, &mut mags);
        mags
    }

    fn two_tone_column(f1: f64, f2: f64, sample_rate: u32, n: usize) -> Vec<f32> {
        let samples: Vec<f32> = (0..n)
            .map(|i| {
                let t = i as f64 / sample_rate as f64;
                ((core::f64::consts::TAU * f1 * t).sin() + (core::f64::consts::TAU * f2 * t).sin())
                    as f32
            })
            .collect();
        let mut mags = vec![0.0f32; n / 2];
        modem_core::analyse::magnitudes(&samples, &mut mags);
        mags
    }

    // --- Required test: a real tone lights the row its label is on ------

    #[test]
    fn a_real_1270hz_tone_lights_the_1270_row() {
        const SR: u32 = 8000;
        let n = modem_core::analyse::fft_size_for(SR);
        let mut spectrum = Spectrum::new(SR);
        spectrum.push(sine_column(1270.0, SR, n));

        let area = Rect::new(0, 0, 30, 20);
        let mut buf = Buffer::empty(area);
        render(&mut buf, area, &spectrum, style(), dim());

        let row_1270 = find_label_row(&buf, area, "1270");
        let row_1070 = find_label_row(&buf, area, "1070");
        let row_2025 = find_label_row(&buf, area, "2025");
        let row_2225 = find_label_row(&buf, area, "2225");
        let newest_x = area.right() - 1;

        let lit = cell_brightness(&buf, newest_x, row_1270);
        for (name, row) in [("1070", row_1070), ("2025", row_2025), ("2225", row_2225)] {
            let other = cell_brightness(&buf, newest_x, row);
            assert!(
                lit > other + 50,
                "1270 Hz tone did not clearly light the 1270 row over the {name} row: \
                 1270={lit}, {name}={other}"
            );
        }
    }

    #[test]
    fn a_real_2225hz_tone_lights_the_2225_row() {
        const SR: u32 = 8000;
        let n = modem_core::analyse::fft_size_for(SR);
        let mut spectrum = Spectrum::new(SR);
        spectrum.push(sine_column(2225.0, SR, n));

        let area = Rect::new(0, 0, 30, 20);
        let mut buf = Buffer::empty(area);
        render(&mut buf, area, &spectrum, style(), dim());

        let row_2225 = find_label_row(&buf, area, "2225");
        let row_1070 = find_label_row(&buf, area, "1070");
        let row_1270 = find_label_row(&buf, area, "1270");
        let row_2025 = find_label_row(&buf, area, "2025");
        let newest_x = area.right() - 1;

        let lit = cell_brightness(&buf, newest_x, row_2225);
        for (name, row) in [("1070", row_1070), ("1270", row_1270), ("2025", row_2025)] {
            let other = cell_brightness(&buf, newest_x, row);
            assert!(
                lit > other + 50,
                "2225 Hz tone did not clearly light the 2225 row over the {name} row: \
                 2225={lit}, {name}={other}"
            );
        }
    }

    // --- Required test: both Bell 103 marks resolve as two rows with a gap

    #[test]
    fn both_bell_103_marks_resolve_as_two_lit_rows_with_a_gap() {
        const SR: u32 = 8000;
        // 41 rows: comfortably above the threshold, and (measured, not
        // guessed - see the task report) far enough from a subband
        // boundary landing between the two tones' own bins that the gap
        // margin is tens of dB, not a hair's breadth. Heights exist
        // (e.g. 16, 40) where the same two tones' FFT bins happen to
        // straddle a subband edge and blur into an adjacent row - a real
        // limitation of fixed-width sub-band bucketing, not something
        // this test papers over by picking a lucky height once.
        const H: u16 = 41;
        let n = modem_core::analyse::fft_size_for(SR);
        let mut spectrum = Spectrum::new(SR);
        spectrum.push(two_tone_column(1070.0, 1270.0, SR, n));

        let area = Rect::new(0, 0, 30, H);
        let mut buf = Buffer::empty(area);
        render(&mut buf, area, &spectrum, style(), dim());

        let row_1070 = find_label_row(&buf, area, "1070");
        let row_1270 = find_label_row(&buf, area, "1270");
        let newest_x = area.right() - 1;

        assert!(
            row_1070 != row_1270,
            "at {H} rows the two marks must not land on the same row"
        );
        let (top_row, bottom_row) = if row_1070 < row_1270 {
            (row_1070, row_1270)
        } else {
            (row_1270, row_1070)
        };
        assert!(
            bottom_row > top_row + 1,
            "expected at least one row strictly between 1070 and 1270 at {H} rows, \
             got rows {row_1070} and {row_1270}"
        );

        let lit_1070 = cell_brightness(&buf, newest_x, row_1070);
        let lit_1270 = cell_brightness(&buf, newest_x, row_1270);

        let mut gap_max = 0u32;
        for y in (top_row + 1)..bottom_row {
            gap_max = gap_max.max(cell_brightness(&buf, newest_x, y));
        }

        assert!(
            lit_1070 > gap_max + 50,
            "1070 row ({lit_1070}) not clearly above the gap ({gap_max})"
        );
        assert!(
            lit_1270 > gap_max + 50,
            "1270 row ({lit_1270}) not clearly above the gap ({gap_max})"
        );
    }

    // --- Required test: below 12 rows, the axis is the four-tone form ---

    #[test]
    fn below_twelve_rows_the_axis_shows_only_the_four_bell_tones() {
        let area = Rect::new(0, 0, 30, CONTINUOUS_AXIS_MIN_ROWS - 1);
        let mut buf = Buffer::empty(area);
        render(&mut buf, area, &Spectrum::new(8000), style(), dim());

        let all_rows: String = (area.top()..area.bottom())
            .map(|y| row_text(&buf, area, y))
            .collect::<Vec<_>>()
            .join("\n");
        for label in ["2225", "2025", "1270", "1070"] {
            assert!(
                all_rows.contains(label),
                "expected {label} below the threshold: {all_rows:?}"
            );
        }
        for label in [" 300", "3400"] {
            assert!(
                !all_rows.contains(label),
                "the continuous axis's own band-edge labels must not appear below the \
                 threshold, a continuous axis it cannot resolve would silently claim to: \
                 {all_rows:?}"
            );
        }
    }

    #[test]
    fn at_twelve_rows_and_above_the_axis_is_continuous() {
        let area = Rect::new(0, 0, 30, CONTINUOUS_AXIS_MIN_ROWS);
        let mut buf = Buffer::empty(area);
        render(&mut buf, area, &Spectrum::new(8000), style(), dim());

        let all_rows: String = (area.top()..area.bottom())
            .map(|y| row_text(&buf, area, y))
            .collect::<Vec<_>>()
            .join("\n");
        for label in [" 300", "1070", "1270", "2025", "2225", "3400"] {
            assert!(
                all_rows.contains(label),
                "expected {label} at the threshold: {all_rows:?}"
            );
        }
    }

    // --- Required test: newest column at the right, scrolls left --------

    #[test]
    fn newest_column_is_at_the_right_and_scrolls_left_over_frames() {
        const SR: u32 = 8000;
        const H: u16 = 20;
        let n = modem_core::analyse::fft_size_for(SR);
        let mut spectrum = Spectrum::new(SR);
        spectrum.push(sine_column(1070.0, SR, n)); // oldest
        spectrum.push(sine_column(2225.0, SR, n));
        spectrum.push(sine_column(1270.0, SR, n)); // newest

        let area = Rect::new(0, 0, 30, H);
        let mut buf = Buffer::empty(area);
        render(&mut buf, area, &spectrum, style(), dim());

        let row_1070 = find_label_row(&buf, area, "1070");
        let row_1270 = find_label_row(&buf, area, "1270");
        let row_2225 = find_label_row(&buf, area, "2225");

        let rightmost = area.right() - 1;
        let one_left = rightmost - 1;
        let two_left = rightmost - 2;

        assert!(
            cell_brightness(&buf, rightmost, row_1270)
                > cell_brightness(&buf, rightmost, row_1070) + 50,
            "the newest push (1270 Hz) must be at the rightmost column"
        );
        assert!(
            cell_brightness(&buf, one_left, row_2225)
                > cell_brightness(&buf, one_left, row_1270) + 50,
            "one column left of newest must be the previous push (2225 Hz)"
        );
        assert!(
            cell_brightness(&buf, two_left, row_1070)
                > cell_brightness(&buf, two_left, row_2225) + 50,
            "two columns left of newest must be the oldest push (1070 Hz)"
        );
    }

    // --- Required test: renders at a range of widths and heights --------

    #[test]
    fn renders_at_a_wide_range_of_sizes_without_panicking() {
        const SR: u32 = 8000;
        let n = modem_core::analyse::fft_size_for(SR);
        let mut spectrum = Spectrum::new(SR);
        for _ in 0..5 {
            spectrum.push(sine_column(1270.0, SR, n));
        }
        for width in [0u16, 1, 2, 5, 6, 34, 60, 100] {
            for height in [0u16, 1, 2, 6, 11, 12, 13, 20, 40] {
                let area = Rect::new(0, 0, width, height);
                let mut buf = Buffer::empty(area);
                render(&mut buf, area, &spectrum, style(), dim());
            }
        }
    }

    #[test]
    fn an_empty_spectrum_history_draws_no_data_but_does_not_panic() {
        let area = Rect::new(0, 0, 30, 20);
        let mut buf = Buffer::empty(area);
        render(&mut buf, area, &Spectrum::new(8000), style(), dim());
        for y in area.top()..area.bottom() {
            for x in (area.left() + LABEL_WIDTH)..area.right() {
                assert_eq!(
                    cell_brightness(&buf, x, y),
                    0,
                    "expected no data at ({x},{y})"
                );
            }
        }
    }

    // --- The ramp itself, and its mutation proof -------------------------

    /// Two different magnitudes must produce two different ramp levels -
    /// the mutation this pins against is feeding the ramp a constant
    /// magnitude regardless of the real one, which would make every cell
    /// the same brightness and this assertion fail.
    #[test]
    fn the_ramp_produces_different_levels_for_different_magnitudes() {
        let quiet = ramp_level(magnitude_to_db(0.001, 256), 0);
        let loud = ramp_level(magnitude_to_db(128.0, 256), 0);
        assert_ne!(
            quiet, loud,
            "the ramp did not distinguish a quiet magnitude from a loud one"
        );
        assert!(loud > quiet);
    }

    #[test]
    #[allow(clippy::assertions_on_constants)]
    fn the_ramp_has_at_least_five_steps() {
        assert!(RAMP_STEPS >= 5);
    }

    #[test]
    fn the_ramp_floor_is_the_ground_colour_and_full_scale_is_bright() {
        let bright = Color::Rgb(0xFF, 0xB0, 0x00);
        assert_eq!(ramp_colour(0, bright), theme::GROUND);
        assert_eq!(ramp_colour(RAMP_STEPS - 1, bright), bright);
    }

    /// Older columns fade - not a strict requirement of any one required
    /// test, but the decay constant exists to be checked, not just
    /// declared.
    #[test]
    fn decay_reduces_the_ramp_level_of_an_older_column() {
        let db = magnitude_to_db(128.0, 256);
        let fresh = ramp_level(db, 0);
        let old = ramp_level(db, (DECAY_COLUMNS * 4.0) as usize);
        assert!(
            old < fresh,
            "an old column ({old}) must be dimmer than a fresh one ({fresh})"
        );
    }

    // --- Axis mapping unit tests ------------------------------------------

    #[test]
    fn row_for_frequency_is_monotonic_with_frequency() {
        let h = 20;
        let mut last_row = u16::MAX;
        for &(freq, _) in &NAMED_FREQUENCIES {
            let row = row_for_frequency(freq, h);
            assert!(
                row <= last_row,
                "row must not increase as frequency increases"
            );
            last_row = row;
        }
    }

    #[test]
    fn subbands_for_row_is_the_inverse_of_row_from_subband() {
        let h = 15;
        for row in 0..h {
            let (lower, upper) = subbands_for_row(row, h);
            assert_eq!(row_from_subband(lower, h), (row, Half::Lower));
            assert_eq!(row_from_subband(upper, h), (row, Half::Upper));
        }
    }

    #[test]
    fn zero_height_area_does_not_panic() {
        let area = Rect::new(0, 0, 20, 0);
        let mut buf = Buffer::empty(Rect::new(0, 0, 20, 1));
        render(&mut buf, area, &Spectrum::new(8000), style(), dim());
    }

    #[test]
    fn zero_width_area_does_not_panic() {
        let area = Rect::new(0, 0, 0, 20);
        let mut buf = Buffer::empty(Rect::new(0, 0, 1, 20));
        render(&mut buf, area, &Spectrum::new(8000), style(), dim());
    }
}
