//! The well-shaped hole Task 16's real waterfall drops into.
//!
//! Task 16 builds a radix-2 FFT in `modem-core`, Hann-windowed, rendered
//! with half-block characters and a phosphor decay ramp. None of that
//! exists yet, so [`render`] here is deliberately dumb: it takes a
//! [`Spectrum`] as a plain argument (never owns or computes one - see
//! that module's own doc), bins it down to one row per frequency label
//! and draws a single-intensity bar for each. Task 16 replaces this
//! function's body wholesale; it does not need to replace anything that
//! calls it, because the call site already passes the spectrum in rather
//! than reaching for one itself.
//!
//! # Why this boundary is what makes rule 1 checkable
//!
//! "Both panes render the same spectrum" (see the design spec's
//! "Split-screen demo mode" section) is only a testable claim because
//! `render` has no way to fetch its own data - the caller ([`crate::app::App`])
//! holds exactly one `Spectrum` and is the only thing that decides what
//! gets passed to each pane's call. A test that renders the split layout
//! and diffs the two waterfall areas is really testing `App`, not this
//! function - see `app.rs`'s own tests, including the mutation proof that
//! feeds the two panes deliberately different spectra and confirms the
//! diff test catches it.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;

use crate::draw;
use crate::spectrum::Spectrum;

/// The frequency labels down the left edge, top to bottom - the same
/// order and the same four Bell 103 tones plus the band edges the design
/// spec names for the real waterfall. Right-aligned to a fixed width so
/// the bars all start in the same column regardless of digit count.
pub const FREQUENCY_LABELS: [&str; 6] = ["3400", "2225", "2025", "1270", "1070", " 300"];

/// The overture's stages, as the axis footer names them - a simplified,
/// user-facing grouping of `overture::Stage`'s own finer-grained states
/// (no separate off-hook/CI/CM-JM/CJ segments), matching the single-pane
/// mockup's own wording exactly. Task 16 owns turning this into a real
/// time axis; this is static furniture until then.
pub const STAGE_AXIS_LABELS: [&str; 6] =
    ["dial tone", "DTMF", "ringback", "ANSam", "training", "data"];

/// Width in cells the frequency label column occupies, including its
/// trailing separating space, before the bar itself starts.
pub const LABEL_WIDTH: u16 = 5;

/// Resamples `bins` down to exactly [`FREQUENCY_LABELS`]`.len()` values by
/// averaging even-sized chunks - a placeholder mapping, not a real
/// frequency-to-bin correspondence (there is no FFT yet to make that
/// claim true). Never panics: an empty `bins` (which [`Spectrum`] itself
/// should not produce, but this function does not trust that) yields all
/// zeros rather than dividing by zero.
fn bucket(bins: &[f32], count: usize) -> Vec<f32> {
    if bins.is_empty() || count == 0 {
        return vec![0.0; count];
    }
    let mut out = Vec::with_capacity(count);
    for i in 0..count {
        let start = i * bins.len() / count;
        let end = ((i + 1) * bins.len() / count)
            .max(start + 1)
            .min(bins.len());
        let slice = &bins[start..end];
        let avg = slice.iter().copied().sum::<f32>() / slice.len() as f32;
        out.push(avg);
    }
    out
}

/// Draws one frequency-labelled bar row per [`FREQUENCY_LABELS`] entry
/// into `area`, using `spectrum` as the only source of what the bars
/// show. `area` is expected to be [`FREQUENCY_LABELS`]`.len()` rows tall;
/// fewer rows just draws fewer bars, top to bottom, and more rows leaves
/// the remainder untouched - `frame::plan_rows` is what actually decides
/// how many rows a caller has to give this.
pub fn render(buf: &mut Buffer, area: Rect, spectrum: &Spectrum, style: Style, dim: Style) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let rows = (area.height as usize).min(FREQUENCY_LABELS.len());
    let levels = bucket(&spectrum.bins, FREQUENCY_LABELS.len());

    let bar_x = area.left().saturating_add(LABEL_WIDTH);
    let bar_width = area.right().saturating_sub(bar_x);

    for (i, label) in FREQUENCY_LABELS.iter().enumerate().take(rows) {
        let y = area.top() + i as u16;
        draw::text(buf, area, area.left(), y, label, dim);
        if bar_width == 0 {
            continue;
        }
        let level = levels[i].clamp(0.0, 1.0);
        let filled = (level * bar_width as f32).round() as u16;
        let filled = filled.min(bar_width);
        for x in 0..filled {
            draw::cell(buf, area, bar_x + x, y, '\u{2588}', style); // █
        }
    }
}

/// Draws the overture stage-name axis footer under the bars, one row,
/// truncated (never wrapped or panicking) to whatever width is actually
/// available. Only called in single-pane mode - see this module's own
/// doc and the design spec's split mockup, which has no room for it.
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

    #[test]
    fn bucket_of_empty_bins_is_all_zero_not_a_panic() {
        assert_eq!(bucket(&[], 6), vec![0.0; 6]);
    }

    #[test]
    fn a_full_scale_spectrum_fills_the_whole_bar_width() {
        let area = Rect::new(0, 0, 20, 6);
        let mut buf = Buffer::empty(area);
        let spectrum = Spectrum { bins: vec![1.0; 6] };
        render(&mut buf, area, &spectrum, style(), dim());
        let bar_width = area.width - LABEL_WIDTH;
        for y in 0..6 {
            let row = row_text(&buf, area, y);
            let bar: String = row.chars().skip(LABEL_WIDTH as usize).collect();
            assert_eq!(
                bar,
                "\u{2588}".repeat(bar_width as usize),
                "row {y} was not fully filled by a full-scale spectrum"
            );
        }
    }

    #[test]
    fn a_silent_spectrum_draws_no_bar_at_all() {
        let area = Rect::new(0, 0, 20, 6);
        let mut buf = Buffer::empty(area);
        let spectrum = Spectrum::silent(6);
        render(&mut buf, area, &spectrum, style(), dim());
        for y in 0..6 {
            let row = row_text(&buf, area, y);
            let bar: String = row.chars().skip(LABEL_WIDTH as usize).collect();
            assert!(
                bar.chars().all(|c| c == ' '),
                "row {y} drew a bar for a silent spectrum: {bar:?}"
            );
        }
    }

    /// Position, not just presence: the frequency labels must appear in
    /// this exact top-to-bottom order, each on its own row, at column 0 -
    /// a test that only checked "3400 appears somewhere" would pass a
    /// version that drew the labels in the wrong order or in the bar
    /// column instead of the label column.
    #[test]
    fn frequency_labels_appear_in_order_at_the_left_edge() {
        let area = Rect::new(0, 0, 20, 6);
        let mut buf = Buffer::empty(area);
        let spectrum = Spectrum::silent(6);
        render(&mut buf, area, &spectrum, style(), dim());
        for (i, label) in FREQUENCY_LABELS.iter().enumerate() {
            let row = row_text(&buf, area, i as u16);
            assert!(
                row.starts_with(label),
                "row {i} did not start with {label:?}: {row:?}"
            );
        }
    }

    #[test]
    fn zero_height_area_does_not_panic() {
        let area = Rect::new(0, 0, 20, 0);
        let mut buf = Buffer::empty(Rect::new(0, 0, 20, 1));
        render(&mut buf, area, &Spectrum::silent(6), style(), dim());
    }

    #[test]
    fn zero_width_area_does_not_panic() {
        let area = Rect::new(0, 0, 0, 6);
        let mut buf = Buffer::empty(Rect::new(0, 0, 1, 6));
        render(&mut buf, area, &Spectrum::silent(6), style(), dim());
    }
}
