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
import { ensurePlaybackAudioSession, withTimeout, AudioDiagnostics } from './audio-diagnostics.js';

export { Duplex, Role, SessionState, STAGE_NAMES };

const RESUME_TIMEOUT_MS = 4000;
/**
 * How long to wait for the worklet to report its Session ready before
 * giving up on it.
 *
 * `ready` used to be awaited bare, and nothing on the other side is
 * guaranteed to settle it: `processorerror` only records a diagnostic, it
 * does not reject. So a worklet that was constructed but never ran - a
 * rendering thread killed after an interruption, a compile that never
 * finished - left `init` pending for ever, and with it the whole start
 * sequence. No panel, no error, no timeout: the demo simply never
 * appeared, which is indistinguishable from a dead button.
 *
 * Ten seconds is deliberately generous: this waits on a WASM compile
 * inside the worklet on whatever phone is running it, which is slow and
 * legitimately variable. The number exists to convert "hangs for ever"
 * into "says what happened", not to police performance.
 */
const WORKLET_READY_TIMEOUT_MS = 10000;

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
    // Exposed for page.js's Sound help panel - see audio-diagnostics.js.
    this.diagnostics = new AudioDiagnostics('wired');
  }

  /**
   * Fetches the WASM bytes, brings up the AudioWorklet and waits for
   * both Sessions to be ready. Needs no microphone and no permission
   * prompt - the two ends never touch a real input device, only each
   * other.
   *
   * The two default URLs are resolved against **this module's own URL**,
   * not the document's. That distinction is the whole bug fixed on
   * 9 Sep 2026: they used to be the bare strings `'modem.wasm'` and
   * `'wired-worklet.js'`, which a `fetch` resolves against the current
   * document URL - and `page.js`'s Demo button calls
   * `history.pushState(null, '', '/demo/')` *before* calling this. So
   * from the landing page the fetch went to `/demo/modem.wasm` and 404ed,
   * while a direct link to `/demo/` worked, because the deploy injects
   * `<base href="/">` into the route directories and that put the
   * relative URL back at the root. Two paths to the same screen, one
   * working and one not, with nothing on the page to say why.
   *
   * `import.meta.url` cannot be moved by `pushState`, a `<base>` tag, or
   * which route the visitor came in on. The modules and the assets ship
   * side by side, so this is always the right answer.
   *
   * @param {{wasmUrl?: string, workletUrl?: string, duplex?: number}} opts
   */
  async init({
    wasmUrl = new URL('modem.wasm', import.meta.url).href,
    workletUrl = new URL('wired-worklet.js', import.meta.url).href,
    duplex = Duplex.HALF_PING_PONG,
  } = {}) {
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
    // WebKit's ambient-vs-playback session category (see
    // audio-diagnostics.js's own doc) - set before the context is even
    // constructed, since it configures the page's audio session as a
    // whole rather than anything owned by this one context.
    const sessionResult = ensurePlaybackAudioSession();
    if (!sessionResult.ok) this.diagnostics.log(`audioSession not set to 'playback': ${sessionResult.reason}`);
    const ctx = new AudioContextCtor();
    // Published immediately, not at the end of `init`, because two things
    // reach for it during the seconds this function then spends fetching
    // WASM and loading the worklet:
    //
    //   - the page's own foreground handler calls `resumeIfSuspended()`
    //     on visibilitychange. With `this.ctx` still null it did nothing,
    //     so a phone backgrounded and restored during that window
    //     consumed its one resume attempt and carried on with a
    //     suspended context.
    //   - `stop()` can only close `this.ctx`. A fetch or `addModule`
    //     rejection before the old assignment left the context alive with
    //     nothing holding it, so repeated failed starts accumulated
    //     AudioContexts until a phone hit its limit.
    //
    // Both were found by Codex reviewing the mobile flakiness, 10 Sep
    // 2026. Assigning here costs nothing: every other use is guarded on
    // the fields set further down.
    this.ctx = ctx;
    this.diagnostics.recordState(ctx);
    // Must happen here - the very first statement after construction,
    // still synchronous, before this function's first `await` - or the
    // user gesture that led to this call is spent. Desktop Chrome
    // auto-runs a context created inside a gesture even without this
    // call, which is exactly why the one-device demo passed testing on
    // it while staying silently dead on WebKit: no resume() call was
    // ever made at all. The call itself is not awaited here on purpose -
    // only the call needs to land inside the gesture; the raw promise is
    // captured so its rejection reason survives (previously swallowed by
    // an unconditional `.catch(() => {})`) and awaited with a deadline
    // near the end of `init`, once the rest of setup is done, so a
    // caller can read back whether it actually took hold rather than
    // waiting on a promise that might never settle at all (see
    // https://bugs.webkit.org/show_bug.cgi?id=273511).
    const resumePromise = ctx.resume().catch((err) => {
      this.diagnostics.resumeError = String(err && err.message ? err.message : err);
      this.diagnostics.log(`resume() rejected: ${this.diagnostics.resumeError}`);
    });
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
    this.diagnostics.mark('fetched');

    await ctx.audioWorklet.addModule(workletUrl);
    this.diagnostics.mark('moduleAdded');
    // No input needed - the two Sessions are fed from each other, never
    // from a real device - so this node declares zero inputs.
    const node = new AudioWorkletNode(ctx, 'wired-processor', {
      numberOfInputs: 0,
      numberOfOutputs: 1,
      outputChannelCount: [1],
    });
    node.port.onmessage = (e) => this._handleWorkletMessage(e.data);
    // Neither live endpoint listened for this at all before - a
    // processor that throws inside process() (rather than during
    // message handling, which already has its own try/catch) previously
    // vanished with nothing on the port and nothing in the console.
    node.addEventListener('processorerror', (event) => this.diagnostics.onProcessorError(event));

    // `this.ctx` is already set, up where the context was created - see
    // the note there.
    this.node = node;

    const ready = new Promise((resolve, reject) => {
      this._resolveReady = resolve;
      this._rejectReady = reject;
    });
    // Transferred, not copied - see modem.js's identical `init` for why.
    node.port.postMessage({ type: 'wasm', bytes: wasmBytes, duplex }, [wasmBytes]);
    await withTimeout(
      ready,
      WORKLET_READY_TIMEOUT_MS,
      'the audio worklet never reported both Sessions ready - see Sound help for what the pipeline did manage',
    );
    this.diagnostics.mark('wasmReady');

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
    // also resolves if the context was already there). Raced against a
    // deadline rather than awaited bare: a caller (page.js) previously
    // had no way to ever find out if this hung, because nothing here
    // would ever settle to tell it so - the state warning and the panel
    // reveal both sat behind this same await.
    await withTimeout(resumePromise, RESUME_TIMEOUT_MS, `ctx.resume() did not settle within ${RESUME_TIMEOUT_MS}ms`).catch((err) => {
      this.diagnostics.log(String(err));
    });
    this.diagnostics.recordState(ctx);
  }

  /**
   * Re-resumes the context if the page backgrounding suspended it - see
   * spike/README.md finding 5's own point about not leaving a live call
   * running unattended, and iOS's habit of suspending on backgrounding.
   * Also covers WebKit's own `interrupted` state (a phone call or
   * another app taking the audio session - see
   * https://bugs.webkit.org/show_bug.cgi?id=273511), which the
   * `suspended`-only check here used to miss entirely. A no-op if the
   * context is already running or never got this far.
   */
  resumeIfSuspended() {
    if (this.ctx && (this.ctx.state === 'suspended' || this.ctx.state === 'interrupted')) {
      this.ctx.resume().catch((err) => {
        this.diagnostics.resumeError = String(err && err.message ? err.message : err);
        this.diagnostics.log(`resume() rejected (resumeIfSuspended): ${this.diagnostics.resumeError}`);
      });
    }
    this.diagnostics.recordState(this.ctx);
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

  /** @param {'a'|'b'} side - hands that side's turn to the other. */
  yieldTurn(side) {
    this.node.port.postMessage({ type: 'yieldTurn', side });
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
