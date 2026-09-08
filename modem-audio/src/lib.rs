//! Audio I/O for the modem.
//!
//! `modem-core` is `no_std` and performs no I/O by design - see its own
//! module doc. This crate is the boundary where samples actually become
//! bytes on disk (today) or a real sound card (Task 13's `cpal` backend),
//! so it is a normal `std` crate rather than sharing that discipline.
//! Weakening `modem-core` to add file access here would defeat the whole
//! point of the split.

pub mod wav;

pub use wav::{read_wav, write_wav};
