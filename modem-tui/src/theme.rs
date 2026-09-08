//! The amber-phosphor palette, with green and white alternates.
//!
//! `brand/tokens.json` and its generator (plan 2, not this repo) will
//! eventually be the single source for this palette, shared with the
//! `modem.dbhq.uk` page's CSS. Task 15 predates that generator, so the
//! three phosphor colours are pinned here directly from the design spec's
//! own token table (`docs/superpowers/specs/2026-09-07-modem-design.md`,
//! "Brand" section) rather than invented locally - when plan 2 lands,
//! these constants are what `tokens.rs` replaces, not a fresh guess at the
//! same colours.

use ratatui::style::Color;

/// Near-black ground, `#0A0A0A` in the spec's token table.
pub const GROUND: Color = Color::Rgb(0x0A, 0x0A, 0x0A);

/// Phosphor amber, `#FFB000` - the default.
pub const AMBER: Color = Color::Rgb(0xFF, 0xB0, 0x00);

/// Phosphor green, `#33FF33` - the first alternate.
pub const GREEN: Color = Color::Rgb(0x33, 0xFF, 0x33);

/// Phosphor white, the second alternate. Not in the spec's colour table
/// (which names only amber, green and a dim/trace tone), so this is a
/// judgement call: a period terminal's third common phosphor was a
/// near-white P4, rendered here as a warm white rather than pure `#FFFFFF`
/// so it still reads as a phosphor rather than a plain terminal default.
pub const WHITE: Color = Color::Rgb(0xE8, 0xE8, 0xD8);

/// Which phosphor colour is currently selected. `F6` in the brief's
/// function-key bar cycles through these.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Theme {
    #[default]
    Amber,
    Green,
    White,
}

impl Theme {
    /// The full-intensity phosphor colour - used for a focused pane's
    /// border, active text and the carrier indicator.
    pub fn bright(self) -> Color {
        match self {
            Theme::Amber => AMBER,
            Theme::Green => GREEN,
            Theme::White => WHITE,
        }
    }

    /// A reduced-luminance version of the same phosphor, for an
    /// unfocused pane's border and for chrome that should read as present
    /// but secondary (dividers, inactive labels). Not the waterfall's own
    /// decay trace - Task 16 owns that ramp - just this crate's "dimmer"
    /// tone.
    pub fn dim(self) -> Color {
        match self {
            Theme::Amber => Color::Rgb(0x80, 0x58, 0x00),
            Theme::Green => Color::Rgb(0x19, 0x80, 0x19),
            Theme::White => Color::Rgb(0x74, 0x74, 0x6C),
        }
    }

    /// Cycles to the next theme in the order the brief names them: amber,
    /// then green, then white, then back to amber. Bound to `F6`.
    pub fn next(self) -> Theme {
        match self {
            Theme::Amber => Theme::Green,
            Theme::Green => Theme::White,
            Theme::White => Theme::Amber,
        }
    }

    /// The theme's own name, for chrome that wants to say which one is
    /// active. Lowercase, no trailing full stop - a label, not a
    /// sentence (this crate's own style rule).
    pub fn name(self) -> &'static str {
        match self {
            Theme::Amber => "amber",
            Theme::Green => "green",
            Theme::White => "white",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Required-in-spirit: cycling through all three themes returns to the
    /// start, and never repeats early - a `next` that mapped everything to
    /// `Amber` (a constant function) would still "terminate" but would
    /// never show Green or White at all. Checking the full cycle, not just
    /// one call, is what catches that.
    #[test]
    fn theme_cycle_visits_all_three_before_repeating() {
        let mut t = Theme::Amber;
        let mut seen = vec![t];
        for _ in 0..3 {
            t = t.next();
            seen.push(t);
        }
        assert_eq!(
            seen,
            vec![Theme::Amber, Theme::Green, Theme::White, Theme::Amber]
        );
    }

    #[test]
    fn bright_and_dim_are_different_colours_for_every_theme() {
        for theme in [Theme::Amber, Theme::Green, Theme::White] {
            assert_ne!(theme.bright(), theme.dim());
        }
    }
}
