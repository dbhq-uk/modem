//! The white-phosphor palette, with green and amber alternates.
//!
//! Plan 2, task 1 replaced this module's hand-pinned colours with the
//! generated design tokens: `brand/tokens.json` is now the single source,
//! `brand/tokens.css` is the `modem.dbhq.uk` page's copy of the same
//! values, and [`crate::tokens`] is this crate's - both generated from
//! the JSON by `brand/_gen/tokens.py`. Task 15 (before that generator
//! existed) pinned the three phosphor colours here directly from the
//! design spec's own token table
//! (`docs/superpowers/specs/2026-09-07-modem-design.md`, "Brand" section)
//! rather than inventing them locally; that history is why the values
//! below are unchanged even though where they come from is not - the
//! generator's whole job was to prove it could replace them without
//! anything on screen moving.
//!
//! `error()` (`#FF3333`) is exposed alongside the phosphors even though
//! nothing in this crate renders it yet - it is the token set's error
//! colour for a future `NO CARRIER`/error treatment, and it is generated
//! and tested here on the same terms as the rest of the palette.

use ratatui::style::Color;

use crate::tokens;

fn rgb((r, g, b): (u8, u8, u8)) -> Color {
    Color::Rgb(r, g, b)
}

/// Near-black ground - `brand/tokens.json`'s `colour.ground`.
pub const GROUND: Color = Color::Rgb(tokens::GROUND.0, tokens::GROUND.1, tokens::GROUND.2);

/// Phosphor amber - `colour.phosphor.amber.bright`. The design spec's
/// brand direction names amber as the project's colour and that still
/// holds for the wordmark and the page; the terminal itself defaults to
/// white (Dan, 8 Sep 2026).
pub const AMBER: Color = Color::Rgb(
    tokens::AMBER_BRIGHT.0,
    tokens::AMBER_BRIGHT.1,
    tokens::AMBER_BRIGHT.2,
);

/// Phosphor green - `colour.phosphor.green.bright`.
pub const GREEN: Color = Color::Rgb(
    tokens::GREEN_BRIGHT.0,
    tokens::GREEN_BRIGHT.1,
    tokens::GREEN_BRIGHT.2,
);

/// Phosphor white - `colour.phosphor.white.bright`, and the default.
/// Not in the design spec's own colour table (which names only amber,
/// green and a dim/trace tone), so this is a judgement call recorded in
/// `tokens.json` rather than invented here: a period terminal's third
/// common phosphor was a near-white P4, held in the tokens as a warm
/// white rather than pure `#FFFFFF` so it still reads as a phosphor
/// rather than a plain terminal default.
pub const WHITE: Color = Color::Rgb(
    tokens::WHITE_BRIGHT.0,
    tokens::WHITE_BRIGHT.1,
    tokens::WHITE_BRIGHT.2,
);

/// Error red - `colour.error`. See this module's own doc for why it is
/// exposed ahead of any consumer.
pub fn error() -> Color {
    rgb(tokens::ERROR)
}

/// Which phosphor colour is currently selected. `F6` in the brief's
/// function-key bar cycles through these.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Theme {
    #[default]
    White,
    Green,
    Amber,
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
            Theme::Amber => rgb(tokens::AMBER_DIM),
            Theme::Green => rgb(tokens::GREEN_DIM),
            Theme::White => rgb(tokens::WHITE_DIM),
        }
    }

    /// Cycles white, then green, then amber, then back to white - the
    /// order Dan chose on 8 Sep 2026, white first. Bound to `F6`.
    pub fn next(self) -> Theme {
        match self {
            Theme::White => Theme::Green,
            Theme::Green => Theme::Amber,
            Theme::Amber => Theme::White,
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
    /// one colour (a constant function) would still "terminate" but would
    /// never show the other two at all. Checking the full cycle, not just
    /// one call, is what catches that.
    #[test]
    fn theme_cycle_visits_all_three_before_repeating() {
        let mut t = Theme::White;
        let mut seen = vec![t];
        for _ in 0..3 {
            t = t.next();
            seen.push(t);
        }
        assert_eq!(
            seen,
            vec![Theme::White, Theme::Green, Theme::Amber, Theme::White]
        );
    }

    /// The terminal opens white, not amber - Dan, 8 Sep 2026. Asserted
    /// against `Theme::default()` rather than against whatever the app
    /// happens to construct, so a caller passing an explicit theme cannot
    /// hide a changed default.
    #[test]
    fn the_default_phosphor_is_white() {
        assert_eq!(Theme::default(), Theme::White);
    }

    #[test]
    fn bright_and_dim_are_different_colours_for_every_theme() {
        for theme in [Theme::White, Theme::Green, Theme::Amber] {
            assert_ne!(theme.bright(), theme.dim());
        }
    }

    /// `Theme`'s `#[default]` variant and `tokens::DEFAULT_PHOSPHOR` are
    /// two independent statements of the same fact - one a Rust enum
    /// attribute, the other a string generated from `tokens.json`. Only
    /// a human keeps them in step; this is what would catch it if one
    /// changed without the other.
    #[test]
    fn the_default_variant_matches_the_generated_default_phosphor_name() {
        assert_eq!(Theme::default().name(), tokens::DEFAULT_PHOSPHOR);
    }
}
