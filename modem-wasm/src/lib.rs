//! wasm-bindgen surface for the browser endpoint, and a raw C-ABI surface
//! for the AudioWorklet.
//!
//! Task 1 proved the module loads and runs inside an AudioWorklet with a
//! trivial sine generator (`fill_tone_raw`, still here and still used by
//! `spike/`). Task 2 adds the real DSP surface: a raw C-ABI wrapper around
//! `modem_core::session::Session` (see the "worklet's real endpoint"
//! section below), so `web/worklet.js` drives the same Bell 103 session
//! the desktop binary does, not a placeholder.
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

use alloc::boxed::Box;
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

/// Allocate `len` bytes in WASM linear memory and return the pointer.
/// Used for the small, infrequent byte buffers `session_send` and
/// `session_receive_into` copy through - never touched from the audio
/// render loop itself, only from the worklet's message handler.
#[no_mangle]
pub extern "C" fn alloc_u8(len: usize) -> *mut u8 {
    let mut v: ManuallyDrop<Vec<u8>> = ManuallyDrop::new(alloc::vec![0u8; len]);
    v.as_mut_ptr()
}

/// # Safety
/// `ptr` must have come from `alloc_u8` with the same `len`, and must not
/// be used afterwards.
#[no_mangle]
pub unsafe extern "C" fn dealloc_u8(ptr: *mut u8, len: usize) {
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

// ---------------------------------------------------------------------
// The worklet's real endpoint: a raw C-ABI wrapper around
// `modem_core::session::Session`.
//
// Everything below is reachable from the `--no-default-features` build
// the worklet loads - no `#[wasm_bindgen]` in sight, see this module's
// top-level doc for why that matters. A `Session` is heap-allocated once
// via `session_new` and handed back to the caller as an opaque pointer;
// every other function takes that pointer and never allocates one of its
// own on the hot path (`session_process_out`/`session_process_in`, the
// two functions the worklet's `process()` calls every render quantum -
// see finding 4 in spike/README.md). `session_dial`/`session_answer`/
// `session_hangup` do allocate, exactly once each, when `Session`
// rebuilds its `Tx`/`Rx` pair - that only ever happens from the worklet's
// message handler, never from `process()` itself.
use modem_core::overture::Stage;
use modem_core::session::{Session, SessionState};
use modem_core::{Config, Duplex, Role};

/// Builds a fresh, idle [`Session`] and returns an opaque pointer to it.
/// `role` is 0 for [`Role::Originate`], 1 for [`Role::Answer`]; `duplex`
/// is 0 for [`Duplex::Full`], 1 for [`Duplex::HalfPingPong`]. Any other
/// value falls back to `Originate`/`HalfPingPong` respectively rather
/// than panicking - there is no JS exception machinery to catch a panic
/// across this boundary, so an unrecognised code degrades to a defined
/// default instead of aborting the whole module.
#[no_mangle]
pub extern "C" fn session_new(sample_rate: u32, role: u32, duplex: u32) -> *mut Session {
    let cfg = Config {
        sample_rate,
        role: if role == 1 {
            Role::Answer
        } else {
            Role::Originate
        },
        duplex: if duplex == 0 {
            Duplex::Full
        } else {
            Duplex::HalfPingPong
        },
    };
    Box::into_raw(Box::new(Session::new(cfg)))
}

/// Frees a [`Session`] created by `session_new`.
///
/// # Safety
/// `ptr` must have come from `session_new` and must not be used again
/// afterwards.
#[no_mangle]
pub unsafe extern "C" fn session_free(ptr: *mut Session) {
    if !ptr.is_null() {
        drop(Box::from_raw(ptr));
    }
}

/// Starts dialling. `digits_ptr`/`digits_len` name a UTF-8 byte slice;
/// invalid UTF-8 is treated as an empty digit string rather than
/// panicking, matching `session_new`'s "degrade, do not abort" rule.
///
/// # Safety
/// `ptr` must be a live `Session` from `session_new`. `digits_ptr` must
/// point to at least `digits_len` readable bytes.
#[no_mangle]
pub unsafe extern "C" fn session_dial(ptr: *mut Session, digits_ptr: *const u8, digits_len: usize) {
    let session = &mut *ptr;
    let bytes = core::slice::from_raw_parts(digits_ptr, digits_len);
    let digits = core::str::from_utf8(bytes).unwrap_or("");
    session.dial(digits);
}

/// # Safety
/// `ptr` must be a live `Session` from `session_new`.
#[no_mangle]
pub unsafe extern "C" fn session_answer(ptr: *mut Session) {
    (*ptr).answer();
}

/// # Safety
/// `ptr` must be a live `Session` from `session_new`.
#[no_mangle]
pub unsafe extern "C" fn session_hangup(ptr: *mut Session) {
    (*ptr).hangup();
}

/// Fills the next `len` device-rate output samples. Called once per
/// render quantum from the worklet's `process()` - see this module's own
/// doc on why nothing here may allocate.
///
/// # Safety
/// `ptr` must be a live `Session`. `out_ptr` must point to at least `len`
/// writable f32s.
#[no_mangle]
pub unsafe extern "C" fn session_process_out(ptr: *mut Session, out_ptr: *mut f32, len: usize) {
    let session = &mut *ptr;
    let out = core::slice::from_raw_parts_mut(out_ptr, len);
    session.process_out(out);
}

/// Hands the session the next `len` captured device-rate samples.
///
/// # Safety
/// `ptr` must be a live `Session`. `in_ptr` must point to at least `len`
/// readable f32s.
#[no_mangle]
pub unsafe extern "C" fn session_process_in(ptr: *mut Session, in_ptr: *const f32, len: usize) {
    let session = &mut *ptr;
    let input = core::slice::from_raw_parts(in_ptr, len);
    session.process_in(input);
}

/// Queues `len` bytes at `data_ptr` for transmission. Not called from
/// `process()` - only from the worklet's message handler when the page
/// asks to send a chat character, so the allocation inside `Session::send`
/// never lands on the audio thread's per-quantum path.
///
/// # Safety
/// `ptr` must be a live `Session`. `data_ptr` must point to at least
/// `len` readable bytes.
#[no_mangle]
pub unsafe extern "C" fn session_send(ptr: *mut Session, data_ptr: *const u8, len: usize) {
    let session = &mut *ptr;
    let data = core::slice::from_raw_parts(data_ptr, len);
    session.send(data);
}

/// Drains every payload byte the session has received so far and copies
/// up to `cap` of them into `out_ptr`, returning how many bytes were
/// copied.
///
/// This call always fully drains the session's inbox - if more than
/// `cap` bytes were waiting, the excess is discarded, not held over for
/// a later call. That is a deliberate simplification for a 300-baud
/// link: a caller polling every render quantum with a buffer sized in
/// the hundreds of bytes cannot realistically fall behind by more than a
/// handful of bytes, and the alternative (holding a remainder in the
/// session itself) is exactly the kind of unbounded internal buffer this
/// small ABI is trying to avoid. Callers that need a hard guarantee
/// should poll with a generously sized `cap`.
///
/// # Safety
/// `ptr` must be a live `Session`. `out_ptr` must point to at least `cap`
/// writable bytes.
#[no_mangle]
pub unsafe extern "C" fn session_receive_into(
    ptr: *mut Session,
    out_ptr: *mut u8,
    cap: usize,
) -> usize {
    let session = &mut *ptr;
    let data = session.receive();
    let n = data.len().min(cap);
    if n > 0 {
        let out = core::slice::from_raw_parts_mut(out_ptr, cap);
        out[..n].copy_from_slice(&data[..n]);
    }
    n
}

/// The call state: 0 `Idle`, 1 `Dialling`, 2 `Answering`, 3 `Connected`.
///
/// # Safety
/// `ptr` must be a live `Session`.
#[no_mangle]
pub unsafe extern "C" fn session_state(ptr: *mut Session) -> u32 {
    match (*ptr).state() {
        SessionState::Idle => 0,
        SessionState::Dialling => 1,
        SessionState::Answering => 2,
        SessionState::Connected => 3,
    }
}

/// 1 if this end currently holds permission to transmit under half
/// duplex, 0 otherwise.
///
/// # Safety
/// `ptr` must be a live `Session`.
#[no_mangle]
pub unsafe extern "C" fn session_has_turn(ptr: *mut Session) -> u32 {
    (*ptr).has_turn() as u32
}

/// 1 if the receiver currently reports in-band energy above the noise
/// floor, 0 otherwise.
///
/// # Safety
/// `ptr` must be a live `Session`.
#[no_mangle]
pub unsafe extern "C" fn session_carrier_detected(ptr: *mut Session) -> u32 {
    (*ptr).carrier_detected() as u32
}

/// Which end of the call this session currently is: 0 `Originate`, 1
/// `Answer`.
///
/// # Safety
/// `ptr` must be a live `Session`.
#[no_mangle]
pub unsafe extern "C" fn session_role(ptr: *mut Session) -> u32 {
    match (*ptr).role() {
        Role::Originate => 0,
        Role::Answer => 1,
    }
}

/// The overture stage that owned the most recently rendered sample, or
/// -1 outside `Dialling`. Numbered in the order the overture renders
/// them: 0 `OffHook`, 1 `DialTone`, 2 `Dialling`, 3 `Ringback`, 4 `Ci`,
/// 5 `Ansam`, 6 `CmJm`, 7 `Cj`, 8 `Training`, 9 `Connected`. Matched
/// explicitly against [`Stage`]'s variants rather than cast, so a
/// reordering upstream fails this file's own build instead of silently
/// renumbering the wire values a page's JS may already have hardcoded.
///
/// # Safety
/// `ptr` must be a live `Session`.
#[no_mangle]
pub unsafe extern "C" fn session_stage(ptr: *mut Session) -> i32 {
    match (*ptr).stage() {
        None => -1,
        Some(Stage::OffHook) => 0,
        Some(Stage::DialTone) => 1,
        Some(Stage::Dialling) => 2,
        Some(Stage::Ringback) => 3,
        Some(Stage::Ci) => 4,
        Some(Stage::Ansam) => 5,
        Some(Stage::CmJm) => 6,
        Some(Stage::Cj) => 7,
        Some(Stage::Training) => 8,
        Some(Stage::Connected) => 9,
    }
}
