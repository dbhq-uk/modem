//! The shared source both waterfall panes render from.
//!
//! Task 15's `Spectrum` was a single flat list of bins - one column, no
//! history, no notion of a device rate. A scrolling waterfall needs more
//! than a snapshot: it needs the last several columns so the newest one
//! can be drawn at the right edge and the rest scrolled left, and it
//! needs the sample rate a column was computed at so a bin index can be
//! turned into a frequency in Hz at all. Widening the type rather than
//! replacing it keeps the one architectural fact Task 15 built this
//! module to hold open: [`crate::app::App`] holds exactly one `Spectrum`
//! and passes the same reference into both panes' waterfall calls when
//! split, so there is still no per-pane field to drift out of sync. See
//! `crate::waterfall`'s own doc for why that single shared reference is
//! what makes "both panes render the same spectrum" a testable claim at
//! all, and `app.rs`'s own `split_screen_panes_render_identical_waterfalls`
//! test.

use std::collections::VecDeque;

/// How many past columns are kept before the oldest is dropped - enough
/// for any realistic terminal width (a 500-column terminal does not
/// exist) without letting a long-running call grow this without bound.
const DEFAULT_MAX_COLUMNS: usize = 1024;

/// A rolling window of magnitude columns, newest last, plus the sample
/// rate they were computed at.
///
/// Each column is one call's worth of [`modem_core::analyse::magnitudes`]
/// output: bin `k`'s frequency is `k * sample_rate / (2 * bins.len())`.
/// Columns need not all be the same length in principle, but in practice
/// every push in a given run comes from the same `fft_size_for(sample_rate)`,
/// so they are.
#[derive(Clone, Debug, PartialEq)]
pub struct Spectrum {
    columns: VecDeque<Vec<f32>>,
    sample_rate: u32,
    max_columns: usize,
}

impl Spectrum {
    /// An empty history at `sample_rate` - nothing has been pushed yet.
    /// [`crate::waterfall::render`] on an empty `Spectrum` draws no
    /// columns at all, not a panic (see that module's own zero-history
    /// test).
    pub fn new(sample_rate: u32) -> Self {
        Spectrum {
            columns: VecDeque::new(),
            sample_rate,
            max_columns: DEFAULT_MAX_COLUMNS,
        }
    }

    /// As [`Spectrum::new`], but with an explicit column cap - used by
    /// tests that want to prove the cap itself works without pushing a
    /// thousand columns to do it.
    pub fn with_max_columns(sample_rate: u32, max_columns: usize) -> Self {
        Spectrum {
            columns: VecDeque::new(),
            sample_rate,
            max_columns: max_columns.max(1),
        }
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// Appends the newest column, dropping the oldest once `max_columns`
    /// is exceeded.
    pub fn push(&mut self, column: Vec<f32>) {
        self.columns.push_back(column);
        while self.columns.len() > self.max_columns {
            self.columns.pop_front();
        }
    }

    /// How many columns of history are currently held.
    pub fn len(&self) -> usize {
        self.columns.len()
    }

    pub fn is_empty(&self) -> bool {
        self.columns.is_empty()
    }

    /// The column `age` ticks before the newest - `age == 0` is the
    /// newest column (what [`crate::waterfall::render`] draws at the
    /// rightmost position), `age == 1` is one tick older, and so on.
    /// `None` once `age` reaches further back than any history held.
    pub fn column(&self, age: usize) -> Option<&[f32]> {
        let len = self.columns.len();
        if age >= len {
            return None;
        }
        self.columns.get(len - 1 - age).map(Vec::as_slice)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_new_spectrum_has_no_history() {
        let s = Spectrum::new(8000);
        assert!(s.is_empty());
        assert_eq!(s.len(), 0);
        assert_eq!(s.column(0), None);
    }

    #[test]
    fn pushed_columns_are_read_back_newest_first() {
        let mut s = Spectrum::new(8000);
        s.push(vec![1.0, 2.0]);
        s.push(vec![3.0, 4.0]);
        s.push(vec![5.0, 6.0]);
        assert_eq!(s.len(), 3);
        assert_eq!(
            s.column(0),
            Some(&[5.0, 6.0][..]),
            "age 0 must be the newest push"
        );
        assert_eq!(s.column(1), Some(&[3.0, 4.0][..]));
        assert_eq!(
            s.column(2),
            Some(&[1.0, 2.0][..]),
            "age 2 must be the oldest push"
        );
        assert_eq!(s.column(3), None, "no fourth column exists");
    }

    /// The cap must actually drop the *oldest* column, not the newest -
    /// a version that dropped from the wrong end would still shrink to
    /// the right length and pass a test that only checked `len()`.
    #[test]
    fn pushing_past_the_cap_drops_the_oldest_column_not_the_newest() {
        let mut s = Spectrum::with_max_columns(8000, 2);
        s.push(vec![1.0]);
        s.push(vec![2.0]);
        s.push(vec![3.0]);
        assert_eq!(s.len(), 2);
        assert_eq!(s.column(0), Some(&[3.0][..]));
        assert_eq!(s.column(1), Some(&[2.0][..]));
        assert_eq!(
            s.column(2),
            None,
            "the oldest column (1.0) must have been dropped"
        );
    }

    #[test]
    fn sample_rate_is_reported_back() {
        let s = Spectrum::new(48_000);
        assert_eq!(s.sample_rate(), 48_000);
    }
}
