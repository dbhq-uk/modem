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

pub mod analyse;
pub mod at;
pub mod carrier;
pub mod fft;
pub mod frame;
pub mod impair;
pub mod link;
pub mod nco;
pub mod overture;
pub mod resample;
pub mod rx;
pub mod session;
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

/// Which end of the call this is. The bands are split so both directions
/// can carry at once: `Role` names which end you are, not which band you
/// listen on - [`tones`] gives the band this role *transmits* in, and
/// [`Role::listen`] gives the opposite role, whose transmit band is the
/// one this end must listen on.
///
/// # The bug this fixes
///
/// Before Task 12, `Rx::new` called `tones(cfg.role)` to pick its listening
/// band - the same call `Tx::new` makes to pick its transmit band. That is
/// wrong: an originate end transmits on 1270/1070 and listens on
/// 2225/2025, so a `Tx` and an `Rx` built from the *same* role were tuned
/// to the *same* band, not opposite ones. Every test in this project
/// passed anyway, because every loopback fixture hands both ends one
/// shared `Config` - a `Tx` and an `Rx` sharing a role talk to each other
/// happily regardless of which band that role names, so the mix-up was
/// invisible until two independently-configured ends had to talk to each
/// other for real (see `session.rs`'s two-session test, which is the one
/// this bug is visible through).
///
/// The fix: `Config` still carries one `Role` - which end *you* are - and
/// `Rx::new` now calls `tones(cfg.role.listen())` instead of
/// `tones(cfg.role)`. A `Tx` and an `Rx` built from the *same* `Config`
/// (the normal case - one end of a call has one role) now correctly tune
/// to opposite bands, the way a real link works.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Role {
    /// The end that dials.
    Originate,
    /// The end that picks up.
    Answer,
}

impl Role {
    /// The other role. Bell 103 splits the two directions across two
    /// bands, so an end never listens on the band it also transmits in -
    /// `tones(self.listen())` is the band `self` must listen on. This is
    /// an involution: `role.listen().listen() == role`.
    pub fn listen(self) -> Role {
        match self {
            Role::Originate => Role::Answer,
            Role::Answer => Role::Originate,
        }
    }
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
    /// Which end of the call this is. A [`crate::tx::Tx`] built from this
    /// `Config` transmits in `tones(role)`; a [`crate::rx::Rx`] built from
    /// the *same* `Config` listens in `tones(role.listen())` - the other
    /// end's transmit band. One `Config` describes one end completely; it
    /// is not a place to name the far end's role too (see [`Role`]'s own
    /// doc for the bug that shipped before this was made explicit).
    pub role: Role,
    pub duplex: Duplex,
}

/// Mark and space frequencies a role *transmits* in.
///
/// These are Bell 103. They are not V.21, which uses 980/1180 and
/// 1650/1850 and is not interoperable with these.
///
/// This is the transmit band, not necessarily the band to listen on - see
/// [`Role::listen`] for the band a receiver tuned to `role` must use.
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

    /// `listen` must give the *other* role, not the same one back - a
    /// no-op `listen` (returning `self`) would leave `Rx::new`'s
    /// `tones(cfg.role.listen())` identical to the pre-fix
    /// `tones(cfg.role)` and silently reintroduce the bug this method
    /// exists to fix.
    #[test]
    fn listen_gives_the_opposite_role() {
        assert_eq!(Role::Originate.listen(), Role::Answer);
        assert_eq!(Role::Answer.listen(), Role::Originate);
    }

    /// `listen` is its own inverse - applying it twice returns the
    /// starting role. This is what lets a single `Config` describe one
    /// end completely: `Tx` uses `tones(role)`, `Rx` uses
    /// `tones(role.listen())`, and a test fixture that needs "the role
    /// whose *listen* band is X" can recover it as `X.listen()`.
    #[test]
    fn listen_is_its_own_inverse() {
        assert_eq!(Role::Originate.listen().listen(), Role::Originate);
        assert_eq!(Role::Answer.listen().listen(), Role::Answer);
    }
}
