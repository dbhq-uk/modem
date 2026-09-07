//! Rate conversion between the device and the 8 kHz internal rate.
//!
//! Task 7 replaces this with a band-limited, stateful implementation.
//! Until then, two known limitations, both invisible at 8000 Hz where the
//! interpolation and clamp branches coincide:
//!
//! 1. No anti-alias filtering, so content above 4 kHz folds into band on
//!    the way down.
//! 2. Stateless, so fractional position is not carried across calls and
//!    block boundaries are not sample-continuous at rates that do not
//!    divide evenly.
//!
//! Neither corrupts the symbol clock, which lives in Tx and is exact.

pub fn to_device(src: &[f64], dst: &mut [f32], src_rate: f64, dst_rate: f64) {
    let ratio = src_rate / dst_rate;
    for (i, d) in dst.iter_mut().enumerate() {
        let pos = i as f64 * ratio;
        let idx = pos as usize;
        *d = if idx + 1 >= src.len() {
            *src.last().unwrap_or(&0.0) as f32
        } else {
            let frac = pos - idx as f64;
            (src[idx] * (1.0 - frac) + src[idx + 1] * frac) as f32
        };
    }
}

pub fn from_device(src: &[f32], dst: &mut [f64], src_rate: f64, dst_rate: f64) {
    let ratio = src_rate / dst_rate;
    for (i, d) in dst.iter_mut().enumerate() {
        let pos = i as f64 * ratio;
        let idx = pos as usize;
        *d = if idx + 1 >= src.len() {
            *src.last().unwrap_or(&0.0) as f64
        } else {
            let frac = pos - idx as f64;
            src[idx] as f64 * (1.0 - frac) + src[idx + 1] as f64 * frac
        };
    }
}
