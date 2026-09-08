//! Bounds-checked primitives every widget in this crate draws through.
//!
//! `Buffer::set_string`/`Buffer::index` panic if handed a position outside
//! the buffer's own area (see `ratatui-core`'s own doc on
//! `Index<Position> for Buffer`). This crate's whole "render at a range of
//! terminal sizes without panicking" requirement rests on nothing ever
//! calling those directly with a hand-computed coordinate - every write in
//! this crate goes through [`text`] or [`cell`] instead, both of which
//! silently drop a write that would land outside the given area rather
//! than trust the caller's arithmetic.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;

/// Writes `s` at `(x, y)`, clipped to `area` intersected with the
/// buffer's own bounds. A `(x, y)` outside that intersection is a no-op,
/// not a panic - callers are expected to compute `area` correctly, but
/// this is the backstop for when they don't, and it is what keeps a
/// height of 1 or a width of 3 a degraded rendering rather than a crash.
pub(crate) fn text(buf: &mut Buffer, area: Rect, x: u16, y: u16, s: &str, style: Style) {
    let area = area.intersection(buf.area);
    if x < area.left() || x >= area.right() || y < area.top() || y >= area.bottom() {
        return;
    }
    // `Buffer::set_string` alone clips only to the *buffer's* right edge,
    // not to this logical `area` - a narrow pane inside a wide terminal
    // would spill text into its neighbour's column. `set_stringn`'s
    // explicit `max_width` is what actually confines it to `area`.
    let max_width = (area.right() - x) as usize;
    buf.set_stringn(x, y, s, max_width, style);
}

/// Writes one character at `(x, y)`, with the same clipping as [`text`].
pub(crate) fn cell(buf: &mut Buffer, area: Rect, x: u16, y: u16, ch: char, style: Style) {
    let mut tmp = [0u8; 4];
    text(buf, area, x, y, ch.encode_utf8(&mut tmp), style);
}

/// Fills every cell of `area` (already intersected with the buffer) with
/// `ch` styled `style` - used to lay a background down before writing
/// chrome on top of it.
pub(crate) fn fill(buf: &mut Buffer, area: Rect, ch: char, style: Style) {
    let area = area.intersection(buf.area);
    let mut tmp = [0u8; 4];
    let s = ch.encode_utf8(&mut tmp);
    for y in area.top()..area.bottom() {
        for x in area.left()..area.right() {
            buf.set_string(x, y, &*s, style);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::layout::Rect;

    /// Required: a coordinate outside the buffer must never panic, and
    /// must leave the buffer untouched rather than wrapping or clamping
    /// into some other cell.
    #[test]
    fn text_outside_the_buffer_is_a_silent_no_op() {
        let mut buf = Buffer::empty(Rect::new(0, 0, 5, 5));
        let before = buf.clone();
        text(
            &mut buf,
            Rect::new(0, 0, 5, 5),
            100,
            100,
            "x",
            Style::default(),
        );
        assert_eq!(buf, before, "an out-of-range write changed the buffer");
    }

    #[test]
    fn text_clipped_to_a_narrow_area_does_not_spill_into_the_next_column() {
        let mut buf = Buffer::empty(Rect::new(0, 0, 10, 1));
        // area is only columns 0..3; the string is longer than that.
        text(
            &mut buf,
            Rect::new(0, 0, 3, 1),
            0,
            0,
            "HELLO",
            Style::default(),
        );
        assert_eq!(buf[(0, 0)].symbol(), "H");
        assert_eq!(buf[(1, 0)].symbol(), "E");
        assert_eq!(buf[(2, 0)].symbol(), "L");
        // column 3 is outside the 3-wide area passed in, so it must still
        // be whatever an empty buffer cell is - not "L" or "O".
        assert_eq!(buf[(3, 0)].symbol(), " ");
        assert_eq!(buf[(4, 0)].symbol(), " ");
    }
}
