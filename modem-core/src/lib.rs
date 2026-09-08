//! Bell 103 acoustic modem signalling.
//!
//! This crate performs no I/O. It takes sample blocks in and hands sample
//! blocks back, so the same code runs behind a sound card, a browser
//! AudioWorklet, a WAV file or a test.
//!
//! `no_std` is the enforcement mechanism, not an embedded ambition: it makes
//! file and network access a compile error rather than a code-review
//! question, and it guarantees the crate reaches WASM. Tests get `std` back
//! so they can use the usual assertion machinery.
//!
//! Maths comes from `libm` because `core` has no floating-point
//! transcendentals.

#![cfg_attr(not(test), no_std)]

extern crate alloc;

pub mod carrier;
pub mod frame;
pub mod impair;
pub mod link;
pub mod nco;
pub mod resample;
pub mod rx;
pub mod tx;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Internal DSP rate. Device rates are resampled to this so behaviour
/// cannot vary by platform.
pub const DSP_RATE: f64 = 8000.0;

/// Bell 103 symbol rate. Samples per symbol is DSP_RATE/BAUD = 26.6667 and
/// is always handled fractionally.
pub const BAUD: f64 = 300.0;

/// Samples per symbol. Fractional on purpose: rounding to 26 or 27
/// accumulates a whole symbol of drift roughly every forty characters.
#[inline]
pub fn samples_per_symbol() -> f64 {
    DSP_RATE / BAUD
}

/// Which of the two Bell 103 bands this end transmits in. The bands are
/// split so both directions can carry at once.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Role {
    /// The end that dials.
    Originate,
    /// The end that picks up.
    Answer,
}

/// HalfPingPong is the baseline that must work over air. Full is attempted
/// and measured.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Duplex {
    Full,
    HalfPingPong,
}

#[derive(Clone, Copy, Debug)]
pub struct Config {
    /// The audio device's rate, e.g. 48000. Must be greater than zero.
    pub sample_rate: u32,
    pub role: Role,
    pub duplex: Duplex,
}

/// Mark and space frequencies for a role.
///
/// These are Bell 103. They are not V.21, which uses 980/1180 and
/// 1650/1850 and is not interoperable with these.
pub fn tones(role: Role) -> (f64, f64) {
    match role {
        Role::Answer => (2225.0, 2025.0),
        Role::Originate => (1270.0, 1070.0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_is_set() {
        assert!(!VERSION.is_empty());
    }
}
