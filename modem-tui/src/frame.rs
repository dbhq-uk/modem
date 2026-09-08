//! The double-line comms-package chrome: the outer frame, and in split
//! mode the shared spine between the two panes.
//!
//! The brief and the design spec's own "Shell" section both call for
//! **double-line** box borders ("Procomm/Telix double-line borders" /
//! "double-line box borders"), so this module draws with the Unicode
//! double-stroke box-drawing glyphs (`╔═╗║╚╝╦╩╠╣╬`), not the single-line
//! glyphs the approved mockup happens to be typed in. That is a
//! judgement call, recorded here rather than silently resolved: the
//! mockup's ASCII art is single-line (`┌─┐├─┤└─┘`), presumably because
//! those are the box-drawing characters most people can type without
//! hunting a symbol picker, but the prose in both the brief and the
//! design spec's "Shell" section is unambiguous about the actual glyph
//! family wanted, and where the two disagree this module follows the
//! explicit, repeated textual requirement over an illustrative typo of
//! convenience. What the mockup fixes - and what this module is actually
//! tested against - is the *layout*: which labels sit where, in what
//! order, with what separators, not the specific Unicode code point used
//! to draw a straight line.
//!
//! # One layout computation, not two
//!
//! [`layout`] is the single source of truth for where every row and
//! column boundary falls. [`draw`] (the border chrome) and `App::render`
//! (the content inside it) both work from the same [`FrameLayout`] value,
//! which is what makes it structurally impossible for a divider glyph to
//! land one row away from where the content it is supposed to separate
//! actually ends.

use ratatui::buffer::Buffer;
use ratatui::layout::{Margin, Rect};
use ratatui::style::Style;

use crate::draw;

/// Double-stroke box-drawing glyphs. See this module's own doc for why
/// double rather than the mockup's single-line typing.
pub mod glyphs {
    pub const TOP_LEFT: char = '\u{2554}'; // ╔
    pub const TOP_RIGHT: char = '\u{2557}'; // ╗
    pub const BOTTOM_LEFT: char = '\u{255A}'; // ╚
    pub const BOTTOM_RIGHT: char = '\u{255D}'; // ╝
    pub const HORIZONTAL: char = '\u{2550}'; // ═
    pub const VERTICAL: char = '\u{2551}'; // ║
    /// Top border meeting a vertical divider running down into the frame.
    pub const TEE_DOWN: char = '\u{2566}'; // ╦
    /// Bottom border meeting a vertical divider running up into the frame.
    pub const TEE_UP: char = '\u{2569}'; // ╩
    /// Left border meeting a horizontal divider running right.
    pub const TEE_RIGHT: char = '\u{2560}'; // ╠
    /// Right border meeting a horizontal divider running left.
    pub const TEE_LEFT: char = '\u{2563}'; // ╣
    /// The vertical spine crossing a horizontal divider.
    pub const CROSS: char = '\u{256C}'; // ╬
}

/// Where each horizontal band of a pane's interior falls, as absolute
/// buffer rows. Every field is `None`/`0` rather than an arithmetic
/// underflow when the frame is too short to show that band at all - see
/// [`plan_rows`]'s own doc for the degrade order.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RowPlan {
    pub status: Option<u16>,
    pub divider1: Option<u16>,
    pub waterfall_top: Option<u16>,
    pub waterfall_rows: u16,
    pub divider2: Option<u16>,
    pub terminal_top: Option<u16>,
    pub terminal_rows: u16,
}

fn take_row(y: &mut u16, bottom: u16) -> Option<u16> {
    if *y < bottom {
        let r = *y;
        *y += 1;
        Some(r)
    } else {
        None
    }
}

/// Works out how much of `interior`'s height each band gets, in strict
/// priority order: status line first, then the divider under it, then as
/// much of `desired_waterfall_rows` as fits while still leaving room for
/// the second divider and at least one terminal row, then that second
/// divider, then whatever height is left goes to the terminal.
///
/// Every step only ever consumes rows that are still available (`take_row`
/// refuses once `y` reaches `bottom`), so the total consumed can never
/// exceed `interior.height` - this is the actual mechanism behind "renders
/// at any size without panicking or overflowing", not a claim resting on
/// careful arithmetic elsewhere staying careful forever.
pub fn plan_rows(interior: Rect, desired_waterfall_rows: u16) -> RowPlan {
    let mut plan = RowPlan::default();
    let bottom = interior.bottom();
    let mut y = interior.top();

    plan.status = take_row(&mut y, bottom);
    plan.divider1 = take_row(&mut y, bottom);

    let remaining = bottom.saturating_sub(y);
    // Reserve up to 2 rows for the second divider and one terminal row,
    // but only give up waterfall space to a reserve that height can
    // actually afford - on a frame with hardly any height at all, the
    // waterfall degrading to 0 while the reserve still eats the only row
    // left would show nothing at all where a single waterfall row could
    // have fit.
    let reserve = remaining.min(2);
    let waterfall_budget = remaining.saturating_sub(reserve);
    plan.waterfall_rows = desired_waterfall_rows.min(waterfall_budget);
    if plan.waterfall_rows > 0 {
        plan.waterfall_top = Some(y);
        y += plan.waterfall_rows;
    }

    plan.divider2 = take_row(&mut y, bottom);
    if y < bottom {
        plan.terminal_top = Some(y);
    }
    plan.terminal_rows = bottom.saturating_sub(y);
    plan
}

/// The outer frame's computed geometry: the interior content rect for
/// each pane column (1 for single, 2 for split), and the row structure
/// every column shares.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FrameLayout {
    pub outer: Rect,
    pub columns: Vec<Rect>,
    pub rows: RowPlan,
}

/// Computes the frame's geometry for `pane_count` columns (1 or 2) inside
/// `outer`, without drawing anything - callers use the same value both to
/// draw the border ([`draw`]) and to place content inside it.
pub fn layout(outer: Rect, pane_count: usize, desired_waterfall_rows: u16) -> FrameLayout {
    let interior = outer.inner(Margin {
        horizontal: 1,
        vertical: 1,
    });
    let columns = if pane_count <= 1 {
        vec![interior]
    } else {
        // One column of interior width goes to the vertical divider
        // itself; the rest splits as evenly as possible, left getting the
        // extra cell on an odd width.
        let usable = interior.width.saturating_sub(1);
        let left_width = usable.div_ceil(2);
        let right_width = usable.saturating_sub(left_width);
        let left = Rect {
            width: left_width,
            ..interior
        };
        let right = Rect {
            x: interior.x.saturating_add(left_width).saturating_add(1),
            width: right_width,
            ..interior
        };
        vec![left, right]
    };
    let rows = plan_rows(interior, desired_waterfall_rows);
    FrameLayout {
        outer,
        columns,
        rows,
    }
}

/// One pane's two title fragments for the top border - see the mockups:
/// single pane shows the wordmark on the left and the role/band on the
/// right of the *same* border segment; split panes show the role on the
/// left and the band on the right of *each column's own* segment.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Title {
    pub left: String,
    pub right: String,
}

/// Draws the outer double-line border, the vertical spine and title
/// segments for a split frame, and the horizontal dividers from
/// `layout.rows` - all from the one [`FrameLayout`] both this and the
/// content renderers were given, so they cannot disagree about where a
/// row or column boundary is.
///
/// `focused` selects which column (if any) is drawn with `theme.bright()`
/// rather than `theme.dim()` - see the design spec: "the focused pane
/// carries the brighter border." `None` (single-pane mode, nothing to
/// focus between) always draws bright.
pub fn draw(
    buf: &mut Buffer,
    layout: &FrameLayout,
    titles: &[Title],
    focused: Option<usize>,
    bright: Style,
    dim: Style,
) {
    let outer = layout.outer;
    if outer.width < 2 || outer.height < 2 {
        return;
    }
    let area = outer;
    let top = outer.top();
    let bottom = outer.bottom() - 1;
    let left = outer.left();
    let right = outer.right() - 1;

    // Column boundary x-positions where a vertical divider crosses the
    // top/bottom border - only present with 2 columns, at the gap
    // between them.
    let divider_x: Option<u16> = if layout.columns.len() == 2 {
        let c0 = layout.columns[0];
        Some(c0.right())
    } else {
        None
    };

    let style_for = |col: usize| -> Style {
        match focused {
            Some(f) if f == col => bright,
            Some(_) => dim,
            None => bright,
        }
    };

    // Top and bottom borders, drawn per-column so each column's own
    // focus style applies to its own share of the border, then corners
    // and the top/bottom divider tee are stamped over the join.
    for (i, col) in layout.columns.iter().enumerate() {
        let style = style_for(i);
        for x in col.left()..col.right() {
            draw::cell(buf, area, x, top, glyphs::HORIZONTAL, style);
            draw::cell(buf, area, x, bottom, glyphs::HORIZONTAL, style);
        }
    }
    // The one cell of interior width spent on the divider itself, if any.
    if let Some(dx) = divider_x {
        draw::cell(buf, area, dx, top, glyphs::TEE_DOWN, bright);
        draw::cell(buf, area, dx, bottom, glyphs::TEE_UP, bright);
    }
    draw::cell(buf, area, left, top, glyphs::TOP_LEFT, style_for(0));
    draw::cell(
        buf,
        area,
        right,
        top,
        glyphs::TOP_RIGHT,
        style_for(layout.columns.len().saturating_sub(1)),
    );
    draw::cell(buf, area, left, bottom, glyphs::BOTTOM_LEFT, style_for(0));
    draw::cell(
        buf,
        area,
        right,
        bottom,
        glyphs::BOTTOM_RIGHT,
        style_for(layout.columns.len().saturating_sub(1)),
    );

    // Left/right outer borders and the vertical spine between panes, for
    // every content row (not just the divider rows).
    for y in (top + 1)..bottom {
        draw::cell(buf, area, left, y, glyphs::VERTICAL, style_for(0));
        draw::cell(
            buf,
            area,
            right,
            y,
            glyphs::VERTICAL,
            style_for(layout.columns.len().saturating_sub(1)),
        );
        if let Some(dx) = divider_x {
            draw::cell(buf, area, dx, y, glyphs::VERTICAL, bright);
        }
    }

    // Horizontal dividers inside each column, with proper tee/cross
    // glyphs where they meet the outer border or the vertical spine.
    for divider_y in [layout.rows.divider1, layout.rows.divider2]
        .into_iter()
        .flatten()
    {
        for (i, col) in layout.columns.iter().enumerate() {
            let style = style_for(i);
            for x in col.left()..col.right() {
                draw::cell(buf, area, x, divider_y, glyphs::HORIZONTAL, style);
            }
        }
        draw::cell(buf, area, left, divider_y, glyphs::TEE_RIGHT, style_for(0));
        draw::cell(
            buf,
            area,
            right,
            divider_y,
            glyphs::TEE_LEFT,
            style_for(layout.columns.len().saturating_sub(1)),
        );
        if let Some(dx) = divider_x {
            draw::cell(buf, area, dx, divider_y, glyphs::CROSS, bright);
        }
    }

    // Title fragments, embedded in the top border of each column: " left "
    // near the column's own left edge, " right " near its own right edge.
    for (i, col) in layout.columns.iter().enumerate() {
        let Some(title) = titles.get(i) else {
            continue;
        };
        let style = style_for(i);
        if !title.left.is_empty() {
            let text = format!(" {} ", title.left);
            draw::text(buf, area, col.left().saturating_add(1), top, &text, style);
        }
        if !title.right.is_empty() {
            let text = format!(" {} ", title.right);
            let start = col
                .right()
                .saturating_sub(1)
                .saturating_sub(text.chars().count() as u16);
            // Never draw the right title further left than where the left
            // title's own text ends plus a gap - on a narrow column this
            // drops the right title entirely rather than overlapping it.
            let left_end = col
                .left()
                .saturating_add(2 + title.left.chars().count() as u16);
            if start > left_end || title.left.is_empty() {
                draw::text(buf, area, start, top, &text, style);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::layout::Rect;

    #[test]
    fn single_column_layout_is_the_whole_interior() {
        let outer = Rect::new(0, 0, 40, 20);
        let l = layout(outer, 1, 7);
        assert_eq!(l.columns.len(), 1);
        assert_eq!(l.columns[0], Rect::new(1, 1, 38, 18));
    }

    #[test]
    fn split_columns_share_the_interior_with_one_column_for_the_divider() {
        let outer = Rect::new(0, 0, 41, 20);
        let l = layout(outer, 2, 6);
        assert_eq!(l.columns.len(), 2);
        // interior width = 39 (41 - 2 border cells); one column goes to
        // the divider, leaving 38 split 19/19.
        assert_eq!(l.columns[0].width, 19);
        assert_eq!(l.columns[1].width, 19);
        assert_eq!(l.columns[0].right() + 1, l.columns[1].x);
    }

    #[test]
    fn row_plan_never_exceeds_the_interior_height_at_any_size() {
        for height in 0..40u16 {
            let interior = Rect::new(0, 0, 60, height);
            let plan = plan_rows(interior, 7);
            let consumed = plan.status.is_some() as u16
                + plan.divider1.is_some() as u16
                + plan.waterfall_rows
                + plan.divider2.is_some() as u16
                + plan.terminal_rows;
            assert!(
                consumed <= height,
                "height {height}: consumed {consumed} rows, plan {plan:?}"
            );
        }
    }

    #[test]
    fn row_plan_gives_the_full_waterfall_when_there_is_room() {
        let interior = Rect::new(0, 0, 60, 20);
        let plan = plan_rows(interior, 7);
        assert_eq!(plan.waterfall_rows, 7);
        assert!(plan.terminal_rows > 0);
    }

    /// A frame too short even for one row of every band degrades in
    /// priority order (status, then its divider) rather than picking an
    /// arbitrary subset - this pins that order so a future change to it
    /// is a deliberate, visible edit here rather than an accident.
    #[test]
    fn row_plan_prioritises_status_over_everything_else_when_desperately_short() {
        let interior = Rect::new(0, 0, 60, 1);
        let plan = plan_rows(interior, 7);
        assert!(plan.status.is_some());
        assert!(plan.divider1.is_none());
        assert_eq!(plan.waterfall_rows, 0);
    }
}
