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

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_is_set() {
        assert!(!VERSION.is_empty());
    }
}
