// Shared, low-level wrapper around modem-wasm's worklet ABI - the raw
// `extern "C"` exports built with `--no-default-features` (see
// spike/README.md and modem-wasm/src/lib.rs). Nothing in this file
// touches `fetch`, `document` or any other API that is unavailable
// inside AudioWorkletGlobalScope, so it is imported unchanged by both
// `worklet.js` (the audio rendering thread) and `modem.js` / test
// harnesses (the main thread).
//
// The wire encoding here must match modem-wasm/src/lib.rs exactly -
// there is no shared schema, so a change on one side without the other
// fails silently at the ABI boundary rather than at compile time.

export const Role = Object.freeze({ ORIGINATE: 0, ANSWER: 1 });
export const Duplex = Object.freeze({ FULL: 0, HALF_PING_PONG: 1 });
export const SessionState = Object.freeze({
  IDLE: 0,
  DIALLING: 1,
  ANSWERING: 2,
  CONNECTED: 3,
});

// Matches modem-core's overture::Stage, in the order session_stage()
// numbers them. Index -1 (no stage) is represented as `null` by
// SessionHandle.stage(), not as an entry here.
export const STAGE_NAMES = [
  'Off hook',
  'Dial tone',
  'Dialling',
  'Ringback',
  'CI',
  'ANSam',
  'CM/JM',
  'CJ',
  'Training',
  'Connected',
];

/**
 * Compiles and instantiates a worklet-build modem.wasm module from bytes
 * already in hand. Deliberately does not fetch anything itself - fetch
 * and WebAssembly.instantiateStreaming both do not exist inside
 * AudioWorkletGlobalScope (spike/README.md finding 3), so the caller is
 * responsible for getting the bytes here, on whichever thread can
 * actually fetch.
 *
 * The import object is empty on purpose. A module built with the
 * wasm-bindgen surface enabled (the default `browser` feature) declares
 * imports that resolve against generated JS glue that does not exist
 * here, and WASM resolves every declared import at instantiation time
 * regardless of whether anything calls it - so this line is also the
 * proof, every time it runs, that the bytes handed in are genuinely the
 * `--no-default-features` build and not the browser one (finding 1).
 */
export async function instantiateModemModule(bytes) {
  const module = await WebAssembly.compile(bytes);
  const instance = await WebAssembly.instantiate(module, {});
  return instance.exports;
}

/**
 * One end of a call, wrapping the raw pointer `session_new` returns and
 * the handful of scratch buffers it needs. Every buffer is allocated
 * once, in the constructor or on first use at a given length, and only
 * ever reallocated if that length changes - never on every call - which
 * is the discipline spike/README.md finding 4 exists to name: a heap
 * allocation per render quantum on the audio thread is the exact failure
 * this architecture is built to avoid.
 *
 * `processOut`/`processIn` also follow finding 4's other half: the
 * Float32Array view over WASM memory is rebuilt after every call that
 * might have grown linear memory, never cached across one, because
 * growth detaches any view taken before it.
 */
export class SessionHandle {
  /**
   * @param {WebAssembly.Exports} exports - from instantiateModemModule.
   * @param {{sampleRate: number, role: number, duplex?: number, sendCap?: number, recvCap?: number}} opts
   */
  constructor(exports, { sampleRate, role, duplex = Duplex.HALF_PING_PONG, sendCap = 4096, recvCap = 4096 }) {
    this.exports = exports;
    this.ptr = exports.session_new(sampleRate, role, duplex);
    this.sendCap = sendCap;
    this.recvCap = recvCap;
    // Allocated once here, reused by dial/send for as long as this
    // handle lives - see the module doc.
    this.sendPtr = exports.alloc_u8(sendCap);
    this.recvPtr = exports.alloc_u8(recvCap);
    this._outPtr = 0;
    this._outLen = 0;
    this._inPtr = 0;
    this._inLen = 0;
  }

  /** @param {Uint8Array} bytes */
  dial(bytes) {
    this._writeSend(bytes);
    this.exports.session_dial(this.ptr, this.sendPtr, Math.min(bytes.length, this.sendCap));
  }

  answer() {
    this.exports.session_answer(this.ptr);
  }

  hangup() {
    this.exports.session_hangup(this.ptr);
  }

  /** @param {Uint8Array} bytes */
  send(bytes) {
    this._writeSend(bytes);
    this.exports.session_send(this.ptr, this.sendPtr, Math.min(bytes.length, this.sendCap));
  }

  _writeSend(bytes) {
    const n = Math.min(bytes.length, this.sendCap);
    const buf = new Uint8Array(this.exports.memory.buffer, this.sendPtr, this.sendCap);
    buf.set(bytes.subarray(0, n));
  }

  /**
   * Drains and returns whatever payload bytes have arrived so far, as a
   * detached copy (safe to keep after the next WASM call, unlike a raw
   * view over `memory.buffer`). Possibly empty.
   * @returns {Uint8Array}
   */
  receive() {
    const n = this.exports.session_receive_into(this.ptr, this.recvPtr, this.recvCap);
    if (n === 0) return new Uint8Array(0);
    return new Uint8Array(this.exports.memory.buffer, this.recvPtr, n).slice();
  }

  /**
   * Fills `out` (a Float32Array at this session's own sample_rate) with
   * the next block to transmit.
   * @param {Float32Array} out
   */
  processOut(out) {
    this._ensureOutBuf(out.length);
    this.exports.session_process_out(this.ptr, this._outPtr, out.length);
    // Rebuilt after the call - see the module doc on why this cannot be
    // cached across it.
    out.set(new Float32Array(this.exports.memory.buffer, this._outPtr, out.length));
  }

  /**
   * Hands the session the next block of captured samples.
   * @param {Float32Array} input
   */
  processIn(input) {
    this._ensureInBuf(input.length);
    const view = new Float32Array(this.exports.memory.buffer, this._inPtr, input.length);
    view.set(input);
    this.exports.session_process_in(this.ptr, this._inPtr, input.length);
  }

  _ensureOutBuf(len) {
    if (this._outPtr && this._outLen === len) return;
    if (this._outPtr) this.exports.dealloc_f32(this._outPtr, this._outLen);
    this._outPtr = this.exports.alloc_f32(len);
    this._outLen = len;
  }

  _ensureInBuf(len) {
    if (this._inPtr && this._inLen === len) return;
    if (this._inPtr) this.exports.dealloc_f32(this._inPtr, this._inLen);
    this._inPtr = this.exports.alloc_f32(len);
    this._inLen = len;
  }

  /** @returns {number} one of SessionState's values */
  state() {
    return this.exports.session_state(this.ptr);
  }

  hasTurn() {
    return this.exports.session_has_turn(this.ptr) !== 0;
  }

  /** Hands the turn to the far end - see modem-wasm's own doc on why
   * this export exists: under Duplex.HALF_PING_PONG, nothing the far
   * end sends can ever reach the wire until this is called at least
   * once from whichever end is currently holding the turn. */
  yieldTurn() {
    this.exports.session_yield_turn(this.ptr);
  }

  carrierDetected() {
    return this.exports.session_carrier_detected(this.ptr) !== 0;
  }

  /** @returns {number} one of Role's values */
  role() {
    return this.exports.session_role(this.ptr);
  }

  /** @returns {number|null} an index into STAGE_NAMES, or null outside Dialling */
  stage() {
    const s = this.exports.session_stage(this.ptr);
    return s < 0 ? null : s;
  }

  /** @returns {number|null} 1 or 2 while stage() is Ringback, otherwise null */
  ringNumber() {
    const n = this.exports.session_ring_number(this.ptr);
    return n < 0 ? null : n;
  }

  /** Releases the WASM-side Session and its scratch buffers. */
  free() {
    if (this._outPtr) this.exports.dealloc_f32(this._outPtr, this._outLen);
    if (this._inPtr) this.exports.dealloc_f32(this._inPtr, this._inLen);
    this.exports.dealloc_u8(this.sendPtr, this.sendCap);
    this.exports.dealloc_u8(this.recvPtr, this.recvCap);
    this.exports.session_free(this.ptr);
    this.ptr = 0;
  }
}
