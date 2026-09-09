// The main-thread driver for the "one device" live demo: two real
// Sessions cross-wired in software inside one AudioWorklet node, the
// browser mirror of modem-audio/src/transport.rs's WiredTransport. No
// microphone, no second machine - both ends live in this page, and the
// mixed signal plays through the visitor's own speakers so the
// handshake is audible while both transcripts fill in.
//
// Same split as modem.js/worklet.js: everything only the main thread
// can do (fetch, AudioContext.audioWorklet.addModule) lives here;
// everything that has to run on the audio rendering thread lives in
// wired-worklet.js.
import { Duplex, Role, SessionState, STAGE_NAMES } from './session.js';

export { Duplex, Role, SessionState, STAGE_NAMES };

const textEncoder = new TextEncoder();
const textDecoder = new TextDecoder();

export function encode(text) {
  return textEncoder.encode(text);
}

export function decode(bytes) {
  return textDecoder.decode(bytes);
}

/**
 * Both ends of one call, running in the browser, cross-wired in
 * software. Wraps a single AudioContext and a single AudioWorkletNode
 * running two modem-wasm `Session`s (see wired-worklet.js).
 *
 * Events: `status` (detail: `{a, b}`, each `{state, stage, hasTurn,
 * carrier}`, posted only on change), `data` (detail: `{side: 'a'|'b',
 * bytes: Uint8Array}`), `error`.
 */
export class WiredEndpoint extends EventTarget {
  constructor() {
    super();
    this.ctx = null;
    this.node = null;
    // The mixed signal that reaches the speakers, tapped for a page's own
    // waterfall - see `init`'s own doc on why this sits between the node
    // and the destination rather than the node connecting straight there.
    this.analyser = null;
    this._resolveReady = null;
    this._rejectReady = null;
  }

  /**
   * Fetches the WASM bytes, brings up the AudioWorklet and waits for
   * both Sessions to be ready. Needs no microphone and no permission
   * prompt - the two ends never touch a real input device, only each
   * other.
   *
   * @param {{wasmUrl?: string, workletUrl?: string, duplex?: number}} opts
   */
  async init({ wasmUrl = 'modem.wasm', workletUrl = 'wired-worklet.js', duplex = Duplex.HALF_PING_PONG } = {}) {
    // spike/README.md finding 2: plain HTTP gives ctx.audioWorklet ===
    // undefined and a bare TypeError out of addModule.
    if (!window.isSecureContext) {
      throw new Error('AudioWorklet needs a secure context (HTTPS or localhost) - this page is not one');
    }

    // Safari (desktop and every iOS browser - Apple requires all of them
    // to run WebKit, so "Chrome on iOS" is this too) still ships it
    // prefixed on some versions.
    const AudioContextCtor = window.AudioContext || window.webkitAudioContext;
    if (!AudioContextCtor) {
      throw new Error('this browser exposes no AudioContext (or webkitAudioContext) at all');
    }
    const ctx = new AudioContextCtor();
    // Must happen here - the very first statement after construction,
    // still synchronous, before this function's first `await` - or the
    // user gesture that led to this call is spent. Desktop Chrome
    // auto-runs a context created inside a gesture even without this
    // call, which is exactly why the one-device demo passed testing on
    // it while staying silently dead on WebKit: no resume() call was
    // ever made at all. The promise is not awaited here on purpose -
    // only the call itself needs to land inside the gesture. `init`
    // awaits it near the end instead, once the rest of setup is done, so
    // a caller can read back whether it actually took hold (see the
    // `await resumePromise` below and page.js's own use of `ctx.state`).
    const resumePromise = ctx.resume().catch(() => {});
    if (!ctx.audioWorklet) {
      throw new Error('this browser exposes no audioWorklet API even in a secure context');
    }

    // fetch runs on the main thread - it does not exist inside
    // AudioWorkletGlobalScope (finding 3), which is why the bytes are
    // posted in rather than fetched there.
    const response = await fetch(wasmUrl);
    if (!response.ok) {
      throw new Error(`could not fetch ${wasmUrl}: ${response.status} ${response.statusText}`);
    }
    const wasmBytes = await response.arrayBuffer();

    await ctx.audioWorklet.addModule(workletUrl);
    // No input needed - the two Sessions are fed from each other, never
    // from a real device - so this node declares zero inputs.
    const node = new AudioWorkletNode(ctx, 'wired-processor', {
      numberOfInputs: 0,
      numberOfOutputs: 1,
      outputChannelCount: [1],
    });
    node.port.onmessage = (e) => this._handleWorkletMessage(e.data);

    this.ctx = ctx;
    this.node = node;

    const ready = new Promise((resolve, reject) => {
      this._resolveReady = resolve;
      this._rejectReady = reject;
    });
    // Transferred, not copied - see modem.js's identical `init` for why.
    node.port.postMessage({ type: 'wasm', bytes: wasmBytes, duplex }, [wasmBytes]);
    await ready;

    // Always connected: an idle pair transmits silence, never anything
    // unexpected, so nothing is gained by deferring this. Routed through
    // an AnalyserNode rather than straight to the destination, so a page
    // can drive its own waterfall off the real mixed call audio - without
    // this tap the node's output reaches the speakers but nothing else
    // ever sees it, which is exactly the bug that left the "one device"
    // demo's spectrogram dark: connecting straight to destination is not
    // enough, an analyser has to sit in the graph to be read from.
    this.analyser = ctx.createAnalyser();
    this.analyser.fftSize = 2048;
    node.connect(this.analyser);
    this.analyser.connect(ctx.destination);

    // Now that the rest of setup is done, find out whether the resume()
    // fired above actually took hold - `ctx.state` is the only honest
    // answer; a resolved promise does not by itself mean 'running' (it
    // also resolves if the context was already there). A caller (page.js)
    // reads this back to tell a visitor rather than stay silent about it.
    await resumePromise;
  }

  /**
   * Re-resumes the context if the page backgrounding suspended it - see
   * spike/README.md finding 5's own point about not leaving a live call
   * running unattended, and iOS's habit of suspending on backgrounding.
   * A no-op if the context is already running or never got this far.
   */
  resumeIfSuspended() {
    if (this.ctx && this.ctx.state === 'suspended') {
      this.ctx.resume().catch(() => {});
    }
  }

  _handleWorkletMessage(msg) {
    switch (msg.type) {
      case 'ready':
        this._resolveReady?.();
        break;
      case 'error':
        this._rejectReady?.(new Error(msg.message));
        this.dispatchEvent(new CustomEvent('error', { detail: msg.message }));
        break;
      case 'status':
        this.dispatchEvent(new CustomEvent('status', { detail: { a: msg.a, b: msg.b } }));
        break;
      case 'data':
        this.dispatchEvent(new CustomEvent('data', { detail: { side: msg.side, bytes: msg.bytes } }));
        break;
      default:
        break;
    }
  }

  /** Originate dials. @param {string} digits */
  dial(digits) {
    this.node.port.postMessage({ type: 'dial', bytes: encode(digits) });
  }

  /** Answer answers. */
  answer() {
    this.node.port.postMessage({ type: 'answer' });
  }

  /** Ends the call on both ends. */
  hangup() {
    this.node.port.postMessage({ type: 'hangup' });
  }

  /** @param {'a'|'b'} side @param {string} text */
  send(side, text) {
    this.node.port.postMessage({ type: 'send', side, bytes: encode(text) });
  }

  /**
   * Tears down the audio graph and closes the context. See modem.js's
   * `stop()` doc: spike/README.md finding 5 is not only about a
   * microphone - a forgotten AudioWorkletNode left running is the same
   * failure with no microphone in the picture at all, so a page must
   * call this once the demo ends, not rely on `hangup()` alone (an idle
   * Session outputs silence, but the node itself keeps rendering it
   * until the context closes).
   */
  async stop() {
    if (this.node) {
      this.node.disconnect();
    }
    if (this.ctx && this.ctx.state !== 'closed') {
      await this.ctx.close();
    }
  }
}
