// Shared between page.js, wired.js and modem.js - the three places an
// AudioContext gets made - so the fixes for "no audio on iOS" cannot
// drift out of one of them the way the missing resume() call once did
// (see wired.js/modem.js's own doc on that bug). One module, one set of
// checkpoints, one Sound help report, regardless of which button a
// visitor actually pressed.

/**
 * WebKit runs Web Audio in an "ambient" session category by default,
 * which *respects the ringer/silent switch* - a context can be
 * genuinely `running`, actually rendering real samples, and still be
 * inaudible on an iPhone with the switch flipped, with nothing in
 * `ctx.state` ever saying so. `playback` does not respect the switch.
 * See https://bugs.webkit.org/show_bug.cgi?id=237322.
 *
 * `navigator.audioSession` is a page-wide session, not one per
 * AudioContext, and setting `.type` is safe to call more than once -
 * this both is idempotent and does not itself need to run inside a user
 * gesture (it only configures the category; it plays nothing).
 * Feature-detected throughout: only WebKit exposes it at all, and only
 * newer WebKit at that.
 */
let audioSessionResult = null;
let audioSessionType = null;

/**
 * Sets the page-wide session category, and remembers which one, so a
 * later call asking for a *different* category is not swallowed as a
 * repeat.
 *
 * The memo used to be `if (audioSessionResult) return` with no record of
 * the type, which was fine while `playback` was the only category
 * anybody asked for and became a bug the moment it was not - see
 * `ensureCaptureAudioSession`.
 */
function setAudioSession(type) {
  if (audioSessionResult && audioSessionType === type) return audioSessionResult;
  if (!('audioSession' in navigator)) {
    audioSessionResult = { ok: false, type, reason: 'navigator.audioSession is not exposed by this browser' };
    return audioSessionResult;
  }
  try {
    navigator.audioSession.type = type;
    audioSessionType = type;
    audioSessionResult = { ok: true, type, reason: null };
  } catch (err) {
    audioSessionResult = { ok: false, type, reason: String(err) };
  }
  return audioSessionResult;
}

/** Output only - the demo, and explained.html's overture playback. */
export function ensurePlaybackAudioSession() {
  return setAudioSession('playback');
}

/**
 * Microphone *and* speaker - the /originate and /receive endpoints.
 *
 * `playback` is an output-only category on iOS, and asking for capture
 * while it is active fails outright: `getUserMedia` rejects with
 * "AudioSession category is not compatible with audio capture", which is
 * exactly what an iPhone reported on 10 Sep 2026. Because
 * `ensurePlaybackAudioSession` ran at module load on every page, that
 * category was already set before the visitor could press anything, so
 * the receiving modem could never once have opened a microphone on iOS.
 *
 * `play-and-record` is the category that permits both. It has to be set
 * before `getUserMedia`, not after - the category is what the request is
 * checked against.
 *
 * Still the silent-switch-ignoring behaviour that
 * `ensurePlaybackAudioSession`'s own doc describes: `play-and-record`
 * does not respect the ringer switch either, so nothing is given up by
 * moving to it on these two routes.
 */
export function ensureCaptureAudioSession() {
  return setAudioSession('play-and-record');
}

export function audioSessionStatus() {
  return audioSessionResult;
}

/**
 * Races `promise` against a deadline instead of awaiting it forever.
 * `ctx.resume()` can itself simply never settle in some WebKit states
 * (see https://bugs.webkit.org/show_bug.cgi?id=273511) - previously
 * nothing here ever timed out, so a caller's own init() could hang
 * indefinitely, silently skipping the state warning and the panel
 * reveal that were supposed to run right after it.
 */
export function withTimeout(promise, ms, message) {
  let timer;
  const timeout = new Promise((_resolve, reject) => {
    timer = setTimeout(() => reject(new Error(message)), ms);
  });
  return Promise.race([promise, timeout]).finally(() => clearTimeout(timer));
}

/** Root-mean-square of the analyser's current time-domain buffer - a
 * cheap, real measurement of "is anything actually coming out of this
 * graph", independent of and stronger evidence than `ctx.state`. */
export function computeRms(analyser) {
  if (!analyser) return null;
  const data = new Float32Array(analyser.fftSize);
  analyser.getFloatTimeDomainData(data);
  let sumSquares = 0;
  for (let i = 0; i < data.length; i++) sumSquares += data[i] * data[i];
  return Math.sqrt(sumSquares / data.length);
}

const RMS_SILENCE_THRESHOLD = 0.0005;

/**
 * Polls `analyser` for up to `timeoutMs` waiting for genuine signal.
 * Resolves `true` the moment it sees one, `false` if the deadline
 * passes with the context still producing nothing measurable - which
 * is exactly the "ctx.state stays running, nothing reaches the
 * speakers" case the old suspended-only check could never see, because
 * it never looked at the signal itself.
 */
export async function waitForAudibleSignal(ctx, analyser, { timeoutMs = 1500, intervalMs = 150 } = {}) {
  if (!ctx || !analyser) return false;
  const deadline = performance.now() + timeoutMs;
  do {
    if (ctx.state === 'running') {
      const rms = computeRms(analyser);
      if (rms !== null && rms > RMS_SILENCE_THRESHOLD) return true;
    }
    await new Promise((resolve) => setTimeout(resolve, intervalMs));
  } while (performance.now() < deadline);
  return false;
}

/**
 * One of these per audio path (the prerendered overture, the wired
 * one-device demo, the two-device live endpoint). Tracks the
 * checkpoints Codex's review asked for - fetched, module loaded, WASM
 * ready, first processing block, output RMS non-zero - plus a
 * chronological event log and the AudioContext's own state history, so
 * the Sound help panel has something concrete to show rather than a
 * single "it didn't work" line. `currentTimeAdvancing` and
 * `rmsNonZero` are approximations for "first processing block": the
 * audio rendering thread cannot be observed directly from here, but
 * `ctx.currentTime` only moves forward when it is actually rendering
 * blocks, which is the same fact from the other side of the port.
 */
export class AudioDiagnostics {
  constructor(label) {
    this.label = label;
    this.checkpoints = {
      fetched: false,
      moduleAdded: false,
      wasmReady: false,
      currentTimeAdvancing: false,
      rmsNonZero: false,
    };
    this.events = [];
    this.stateHistory = [];
    this.resumeError = null;
    this._start = performance.now();
    this.log('diagnostics started');
  }

  log(text) {
    this.events.push({ t: Math.round(performance.now() - this._start), text });
  }

  mark(name) {
    if (Object.prototype.hasOwnProperty.call(this.checkpoints, name)) {
      this.checkpoints[name] = true;
    }
    this.log(`checkpoint: ${name}`);
  }

  recordState(ctx) {
    if (!ctx) return;
    const last = this.stateHistory[this.stateHistory.length - 1];
    if (!last || last.state !== ctx.state) {
      this.stateHistory.push({ t: Math.round(performance.now() - this._start), state: ctx.state });
    }
  }

  onProcessorError(event) {
    this.log(`processorerror${event && event.message ? `: ${event.message}` : ' (no detail - the event carries none by spec)'}`);
  }
}

// -----------------------------------------------------------------------
// Content-Security-Policy violations, collected globally rather than per
// diagnostics instance - a blocked script or style is a page-wide fact,
// not something tied to whichever demo happened to be running when it
// fired. Attached once; every AudioDiagnostics report pulls from the
// same list.
// -----------------------------------------------------------------------
export const cspViolations = [];
let cspListenerAttached = false;

export function attachSecurityPolicyListener() {
  if (cspListenerAttached) return;
  cspListenerAttached = true;
  document.addEventListener('securitypolicyviolation', (event) => {
    cspViolations.push({
      t: Date.now(),
      directive: event.violatedDirective,
      blockedURI: event.blockedURI,
      sourceFile: event.sourceFile,
      lineNumber: event.lineNumber,
    });
    console.error('CSP violation:', event.violatedDirective, event.blockedURI);
  });
}
