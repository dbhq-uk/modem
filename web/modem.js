// The main-thread driver for the real browser endpoint: fetches the WASM
// module, sets up the AudioWorklet, opens the microphone raw, and
// exposes a small event-based API. Everything that only the main thread
// can do (fetch, getUserMedia, AudioContext.audioWorklet.addModule)
// lives here; everything that has to run on the audio rendering thread
// lives in worklet.js. See spike/README.md for why that split exists at
// all.
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
 * The three settings browsers apply to "clean up" a call, all of which
 * actively work against this modem: they are built to remove exactly
 * the sustained narrowband tones Bell 103 FSK is made of. Exported so a
 * page and this module apply the identical list rather than two
 * hand-maintained copies drifting apart.
 */
export const RAW_MIC_CONSTRAINTS = ['echoCancellation', 'noiseSuppression', 'autoGainControl'];

/**
 * Turns the object `openMicrophone` resolves with into one human-
 * readable line, so a page (or this task's own harness) has something
 * to show without re-deriving it from `applied`/`warnings` itself.
 */
export function summariseMicDiagnostics(diagnostics) {
  if (diagnostics.warnings.length === 0) {
    return 'microphone opened raw - echo cancellation, noise suppression and automatic gain control are all off';
  }
  return `microphone opened with caveats - ${diagnostics.warnings.join('; ')}`;
}

/**
 * One end of a call, running in the browser. Wraps an AudioContext, an
 * AudioWorkletNode running modem-wasm's real `Session`, and (once
 * `openMicrophone` is called) a raw microphone track feeding it.
 *
 * Events: `status` (state/stage/hasTurn/carrier, only on change - see
 * worklet.js), `data` (a Uint8Array of newly received payload bytes),
 * `diagnostic` (the result of `openMicrophone`), `error`.
 */
export class ModemEndpoint extends EventTarget {
  constructor() {
    super();
    this.ctx = null;
    this.node = null;
    this.micStream = null;
    this._resolveReady = null;
    this._rejectReady = null;
  }

  /**
   * Fetches the WASM bytes, brings up the AudioWorklet and waits for the
   * worklet to report its Session is ready. Does not touch the
   * microphone - see `openMicrophone` for that, called separately so a
   * page can dial without ever prompting for one (Task 3's staged
   * design).
   *
   * @param {{wasmUrl?: string, workletUrl?: string, role?: number, duplex?: number}} opts
   */
  async init({ wasmUrl = 'modem.wasm', workletUrl = 'worklet.js', role = Role.ORIGINATE, duplex = Duplex.HALF_PING_PONG } = {}) {
    // spike/README.md finding 2: plain HTTP gives ctx.audioWorklet ===
    // undefined and a bare TypeError out of addModule. Check first and
    // say why, rather than let that unexplained TypeError surface.
    if (!window.isSecureContext) {
      throw new Error('AudioWorklet needs a secure context (HTTPS or localhost) - this page is not one');
    }

    const ctx = new AudioContext();
    if (!ctx.audioWorklet) {
      throw new Error('this browser exposes no audioWorklet API even in a secure context');
    }

    // fetch runs on the main thread. It does not exist inside
    // AudioWorkletGlobalScope, which is why the bytes are posted in
    // rather than fetched there (finding 3).
    const response = await fetch(wasmUrl);
    if (!response.ok) {
      throw new Error(`could not fetch ${wasmUrl}: ${response.status} ${response.statusText}`);
    }
    const wasmBytes = await response.arrayBuffer();

    await ctx.audioWorklet.addModule(workletUrl);
    const node = new AudioWorkletNode(ctx, 'modem-processor', {
      numberOfInputs: 1,
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
    // Transferred, not copied - the worklet needs the actual bytes, and
    // a structured-clone copy of a 20-60 KB buffer is wasted work for
    // something about to be compiled and then never touched by this
    // thread again.
    node.port.postMessage({ type: 'wasm', bytes: wasmBytes, role, duplex }, [wasmBytes]);
    await ready;

    // Always connected: an idle or dialling session transmits silence
    // or the performed overture, never anything unexpected, so nothing
    // is gained by deferring this until later.
    node.connect(ctx.destination);
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
        this.dispatchEvent(new CustomEvent('status', { detail: msg }));
        break;
      case 'data':
        this.dispatchEvent(new CustomEvent('data', { detail: msg.bytes }));
        break;
      default:
        break;
    }
  }

  dial(digits) {
    this.node.port.postMessage({ type: 'dial', bytes: encode(digits) });
  }

  answer() {
    this.node.port.postMessage({ type: 'answer' });
  }

  hangup() {
    this.node.port.postMessage({ type: 'hangup' });
  }

  send(text) {
    this.node.port.postMessage({ type: 'send', bytes: encode(text) });
  }

  /**
   * Asks for the microphone with every processing feature turned off as
   * a required constraint, not a hint - `{exact: false}`, not a plain
   * `false`, because a plain boolean is only a request some hardware
   * and drivers ignore outright. If the device cannot satisfy that
   * (`OverconstrainedError`), falls back to asking as a plain request so
   * the demo can still run in degraded form, and says so.
   *
   * Either way, reads back what the platform actually applied via
   * `MediaStreamTrack.getSettings()` and returns a diagnostics object
   * (`{requestedExact, supported, applied, warnings}`) rather than
   * trusting the request succeeded - some hardware applies its own
   * processing regardless of what either form of constraint asks for.
   * `summariseMicDiagnostics` turns this into one line for display.
   *
   * @returns {Promise<{requestedExact: boolean, supported: object|null, applied: object, warnings: string[]}>}
   */
  async openMicrophone() {
    const diagnostics = { requestedExact: true, supported: null, applied: null, warnings: [] };

    if (!navigator.mediaDevices || !navigator.mediaDevices.getUserMedia) {
      diagnostics.warnings.push('this browser has no getUserMedia at all - no microphone endpoint is possible here');
      this.dispatchEvent(new CustomEvent('diagnostic', { detail: diagnostics }));
      throw new Error('getUserMedia is unavailable');
    }

    if (navigator.mediaDevices.getSupportedConstraints) {
      const supported = navigator.mediaDevices.getSupportedConstraints();
      diagnostics.supported = supported;
      for (const key of RAW_MIC_CONSTRAINTS) {
        if (!supported[key]) {
          diagnostics.warnings.push(
            `this browser does not list ${key} as a supported constraint - it may apply its own processing regardless of what is asked`,
          );
        }
      }
    }

    let stream;
    try {
      stream = await navigator.mediaDevices.getUserMedia({
        audio: {
          echoCancellation: { exact: false },
          noiseSuppression: { exact: false },
          autoGainControl: { exact: false },
        },
      });
    } catch (err) {
      // A required constraint the hardware or driver cannot satisfy
      // throws OverconstrainedError rather than silently degrading -
      // that is a genuine, reportable fact about this device, not a
      // bug to route around quietly. Retry as a plain request instead
      // of a requirement, so the demo can still run in degraded form,
      // and say so rather than leaving somebody watching a demo that
      // cannot work with no explanation.
      diagnostics.requestedExact = false;
      diagnostics.warnings.push(
        `this device would not guarantee raw audio (${err.name}: ${err.message}) - retrying as a request rather than a requirement`,
      );
      stream = await navigator.mediaDevices.getUserMedia({
        audio: {
          echoCancellation: false,
          noiseSuppression: false,
          autoGainControl: false,
        },
      });
    }

    this.micStream = stream;
    const track = stream.getAudioTracks()[0];
    const settings = track.getSettings ? track.getSettings() : {};
    diagnostics.applied = settings;

    for (const key of RAW_MIC_CONSTRAINTS) {
      if (settings[key] === true) {
        diagnostics.warnings.push(
          `${key} is still on for this device - it will suppress the sustained tones this modem uses, and the demo may not decode`,
        );
      }
    }

    const source = this.ctx.createMediaStreamSource(stream);
    source.connect(this.node);

    this.dispatchEvent(new CustomEvent('diagnostic', { detail: diagnostics }));
    return diagnostics;
  }

  /**
   * Stops the microphone and the audio graph and closes the context.
   * spike/README.md finding 5: the first version of this architecture
   * held a carrier on a real speaker until somebody worked out where it
   * was coming from. A page must call this once a call ends - it is not
   * automatic, because an open call is meant to keep running.
   */
  async stop() {
    if (this.micStream) {
      for (const track of this.micStream.getTracks()) track.stop();
      this.micStream = null;
    }
    if (this.node) {
      this.node.disconnect();
    }
    if (this.ctx && this.ctx.state !== 'closed') {
      await this.ctx.close();
    }
  }
}
