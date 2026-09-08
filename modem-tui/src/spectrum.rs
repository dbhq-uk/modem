//! The boundary Task 16's real waterfall drops into.
//!
//! Task 16 builds a radix-2 FFT in `modem-core` and feeds its magnitudes
//! in here. Task 15 does not have that FFT yet, so this type is
//! deliberately thin: a flat list of per-bin magnitudes and nothing else -
//! no source, no ownership of samples, no knowledge of how it was
//! computed. [`crate::waterfall::render`] takes one as a plain argument,
//! which is what makes rule 1 ("both panes render the same spectrum")
//! enforceable at all: the [`crate::app::App`] holds exactly one
//! `Spectrum` and passes the same reference into both panes' waterfall
//! area when split, rather than each pane owning (or computing) its own.
//!
//! Replacing this with the real FFT output in Task 16 does not change
//! this contract - only what fills `bins` and how many of them there are.

/// Magnitude per frequency bin, lowest frequency first, spanning the
/// telephone band (300-3400 Hz per the design spec). See this module's
/// own doc for the contract this type exists to hold open.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Spectrum {
    /// No fixed length is assumed anywhere else in this crate -
    /// [`crate::waterfall::render`] resamples whatever length it is given
    /// down to the number of rows it has to fill.
    pub bins: Vec<f32>,
}

impl Spectrum {
    /// A spectrum with no energy anywhere - Task 15's own placeholder
    /// value before Task 16's FFT exists to fill it with anything real.
    pub fn silent(len: usize) -> Self {
        Spectrum {
            bins: vec![0.0; len.max(1)],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn silent_spectrum_is_all_zero() {
        let s = Spectrum::silent(8);
        assert_eq!(s.bins, vec![0.0; 8]);
    }

    /// `silent(0)` must not produce an empty `bins` - a length-0 spectrum
    /// would divide-by-zero the moment `waterfall::render` tries to
    /// resample it down to its row count.
    #[test]
    fn silent_spectrum_is_never_empty() {
        let s = Spectrum::silent(0);
        assert!(!s.bins.is_empty());
    }
}
