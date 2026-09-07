//! wasm-bindgen surface for the browser endpoint, and a raw C-ABI surface
//! for the AudioWorklet.
//!
//! Task 1 only proves the module loads and runs inside an AudioWorklet.
//! The real DSP surface arrives once modem-core has one.
//!
//! The two surfaces cannot ship in the same compiled artifact. Discovered
//! while building the worklet spike: any `#[wasm_bindgen]` item, even one
//! that never touches a string or an externref, makes wasm-bindgen 0.2.128
//! emit `__wbindgen_describe`, `__wbindgen_object_drop_ref`,
//! `__wbindgen_externref_table_set_null` and
//! `__wbindgen_externref_table_grow` imports, and `fill_tone`'s
//! `&mut [f32]` parameter adds a `__wbg___wbindgen_copy_to_typed_array_...`
//! import on top. Both resolve against `wasm-bindgen`'s generated
//! `./modem_wasm_bg.js` glue, which does not exist inside
//! `AudioWorkletGlobalScope` - so a worklet
//! calling `WebAssembly.instantiate(module, {})` on a build that contains
//! both surfaces fails immediately, whether or not it ever calls the
//! bindgen-wrapped functions. WASM resolves every declared import at
//! instantiation time, not just the ones a caller uses.
//!
//! The `browser` feature (on by default) gates the wasm-bindgen surface.
//! Building with `--no-default-features` drops it, leaving only the raw
//! `extern "C"` exports below and a module with zero imports - the build
//! the worklet loads.

extern crate alloc;

#[cfg(feature = "browser")]
use wasm_bindgen::prelude::*;

fn fill_tone_impl(out: &mut [f32], freq: f32, sample_rate: f32, phase: f32) -> f32 {
    let mut p = phase;
    let step = core::f32::consts::TAU * freq / sample_rate;
    for s in out.iter_mut() {
        *s = p.sin();
        p += step;
        if p > core::f32::consts::TAU {
            p -= core::f32::consts::TAU;
        }
    }
    p
}

/// Fills `out` with a sine at `freq`, advancing from `phase` and returning
/// the new phase. Deliberately trivial - this exists to prove a normal
/// (non-worklet) caller can reach WASM via the usual wasm-bindgen glue.
#[cfg(feature = "browser")]
#[wasm_bindgen]
pub fn fill_tone(out: &mut [f32], freq: f32, sample_rate: f32, phase: f32) -> f32 {
    fill_tone_impl(out, freq, sample_rate, phase)
}

#[cfg(feature = "browser")]
#[wasm_bindgen]
pub fn core_version() -> String {
    modem_core::VERSION.to_string()
}

use alloc::vec::Vec;
use core::mem::ManuallyDrop;

/// Allocate `len` f32s in WASM linear memory and return the pointer.
/// The worklet owns the buffer until it calls `dealloc_f32`.
#[no_mangle]
pub extern "C" fn alloc_f32(len: usize) -> *mut f32 {
    let mut v: ManuallyDrop<Vec<f32>> = ManuallyDrop::new(alloc::vec![0.0; len]);
    v.as_mut_ptr()
}

/// # Safety
/// `ptr` must have come from `alloc_f32` with the same `len`, and must not
/// be used afterwards.
#[no_mangle]
pub unsafe extern "C" fn dealloc_f32(ptr: *mut f32, len: usize) {
    drop(Vec::from_raw_parts(ptr, len, len));
}

/// # Safety
/// `ptr` must point to at least `len` writable f32s.
#[no_mangle]
pub unsafe extern "C" fn fill_tone_raw(
    ptr: *mut f32,
    len: usize,
    freq: f32,
    sample_rate: f32,
    phase: f32,
) -> f32 {
    let out = core::slice::from_raw_parts_mut(ptr, len);
    fill_tone_impl(out, freq, sample_rate, phase)
}
