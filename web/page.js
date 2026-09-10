// The page's own behaviour, extracted from an inline <script> so the
// Content-Security-Policy can stay strict. An inline block would need
// 'unsafe-inline' or a hash that changes every time the file does, and
// web/_gen/frames.py rewrites index.html, so a hash would go stale on
// its own. Blocking this script kills the whole demo, which is exactly
// what happened on the first deploy.
import { Role, Duplex, SessionState, STAGE_NAMES } from './session.js';
import { ModemEndpoint, summariseMicDiagnostics } from './modem.js';
import { WiredEndpoint, decode as decodeWired } from './wired.js';
import {
  ensurePlaybackAudioSession,
  attachSecurityPolicyListener,
  cspViolations,
  computeRms,
  waitForAudibleSignal,
} from './audio-diagnostics.js';
import { Waterfall } from './waterfall.js';
import { appendTerminalLine } from './terminal-line.js';

// The window/unhandledrejection listeners that report a fatal startup
// failure live in boot.js, NOT here.
//
// They used to be the first statements in this file, under a comment
// claiming they were "registered before anything else here can throw".
// That was false in the case that matters most: a `type="module"` script
// runs its imports before a single line of its own body, so if this file
// or any of the six it imports fails to load - a transient mobile
// connection is enough - the body never executes, the listeners are never
// registered, and the page is inert with nothing to say. Codex caught the
// false claim reviewing the mobile flakiness on 10 Sep 2026.
//
// boot.js imports nothing and is loaded ahead of this file, so its
// listeners exist even when this module's own graph never arrives.

// The single fix most likely to matter on iOS - see audio-diagnostics.js's
// own doc - set as early as the module can run, well before any click.
// Harmless and idempotent to call this early: it only configures a
// category, it does not play anything or need a gesture.
ensurePlaybackAudioSession();
attachSecurityPolicyListener();

const reduceMotion = window.matchMedia('(prefers-reduced-motion: reduce)').matches;

const micDiagnostic = document.getElementById('mic-diagnostic');

const soundHelpCheckpoints = document.getElementById('sound-help-checkpoints');
const soundHelpState = document.getElementById('sound-help-state');
const resumeSoundBtn = document.getElementById('resume-sound-btn');
const copyDiagnosticsBtn = document.getElementById('copy-diagnostics-btn');
const copyDiagnosticsStatus = document.getElementById('copy-diagnostics-status');

// -----------------------------------------------------------------------
// Routing: three routes behind one page, plus the plain landing. See the
// "The live routes" section, further down, for the full doc.
// -----------------------------------------------------------------------
const launcher = document.getElementById('launcher');
const launchDemoBtn = document.getElementById('launch-demo-btn');
const launchOriginateBtn = document.getElementById('launch-originate-btn');
const launchReceiveBtn = document.getElementById('launch-receive-btn');
const routeStartBlock = document.getElementById('route-start-block');
const routeStartCopy = document.getElementById('route-start-copy');
const routeStartBtn = document.getElementById('route-start-btn');

const appModeBackBtn = document.getElementById('app-mode-back-btn');
const wiredPanel = document.getElementById('wired-panel');
const wiredPhase = document.getElementById('wired-phase');
const wiredStatusA = document.getElementById('wired-status-a');
const wiredStatusB = document.getElementById('wired-status-b');
const wiredCaptionA = document.getElementById('wired-caption-a');
const wiredCaptionB = document.getElementById('wired-caption-b');
const wiredLogA = document.getElementById('wired-log-a');
const wiredLogB = document.getElementById('wired-log-b');
const wiredCanvas = document.getElementById('wired-waterfall');
const wiredDataCheck = document.getElementById('wired-databcheck');
const wiredDigits = document.getElementById('wired-digits');
const wiredDialBtn = document.getElementById('wired-dial');
const wiredHangupBtn = document.getElementById('wired-hangup');
const wiredStopBtn = document.getElementById('wired-stop');
const wiredSendSide = document.getElementById('wired-send-side');
const wiredChat = document.getElementById('wired-chat');
const wiredSendBtn = document.getElementById('wired-send');

const endpointPanel = document.getElementById('endpoint-panel');
const endpointBackBtn = document.getElementById('endpoint-back-btn');
const endpointHeading = document.getElementById('endpoint-heading');
const endpointIntro = document.getElementById('endpoint-intro');
const shareBlock = document.getElementById('share-block');
const qrCodeEl = document.getElementById('qr-code');
const shareLinkInput = document.getElementById('share-link');
const shareCopyBtn = document.getElementById('share-copy-btn');
const shareCopyStatus = document.getElementById('share-copy-status');
const endpointStatus = document.getElementById('endpoint-status');
const endpointCaption = document.getElementById('endpoint-caption');
const signalIndicators = document.getElementById('signal-indicators');
const micActivityDot = document.getElementById('mic-activity-dot');
const modemSignalDot = document.getElementById('modem-signal-dot');
const endpointDeadline = document.getElementById('endpoint-deadline');
const endpointDataCheck = document.getElementById('endpoint-databcheck');
const endpointHangupBtn = document.getElementById('endpoint-hangup');
const endpointStopBtn = document.getElementById('endpoint-stop');
const chatInput = document.getElementById('chat-input');
const chatSendBtn = document.getElementById('chat-send');
const chatLog = document.getElementById('chat-log');

// The "one device" live demo's own waterfall, driven off WiredEndpoint's
// analyser tap (see wired.js's own doc on why that tap exists) rather
// than a prerendered buffer - this is live audio with no buffer to
// slice. explained.html's own overture playback has its own separate
// Waterfall instance on its own page; the class itself is shared (see
// waterfall.js) so the two can never disagree on how a spectrogram
// reads, but the instances themselves are never shared across pages.
// Built on first use, not at module load, and skipped entirely when the
// canvas is not on this page.
//
// This used to be `const wiredWaterfall = new Waterfall(wiredCanvas)` at
// module scope, and that one line could take the whole page down:
// `Waterfall`'s constructor calls `canvasEl.getContext('2d')`
// immediately, so a null canvas throws `TypeError: Cannot read
// properties of null` **while page.js is still evaluating**. Nothing
// after it runs - which means no click handlers get attached at all, and
// every button on the page silently does nothing. That is the worst
// possible failure shape: no error visible to the visitor, no network
// request, nothing to click that responds. Observed live on 9 Sep 2026,
// where clicking Demo produced no console entry and no request.
//
// A spectrogram is decoration. It must not be able to disable the demo
// it decorates, so it is created lazily and every call site tolerates
// its absence.
let wiredWaterfall = null;
let wiredWaterfallRaf = null;

function ensureWiredWaterfall() {
  if (wiredWaterfall === null && wiredCanvas) {
    try {
      wiredWaterfall = new Waterfall(wiredCanvas);
    } catch (err) {
      console.error('waterfall unavailable, continuing without it', err);
      wiredWaterfall = false;
    }
  }
  return wiredWaterfall || null;
}

function startWiredWaterfall(endpoint) {
  stopWiredWaterfall();
  const waterfall = ensureWiredWaterfall();
  if (!waterfall) return;
  const draw = () => {
    try {
      waterfall.frame(endpoint.analyser, endpoint.ctx.sampleRate);
    } catch (err) {
      console.error('wired waterfall draw failed', err);
      return;
    }
    wiredWaterfallRaf = requestAnimationFrame(draw);
  };
  wiredWaterfallRaf = requestAnimationFrame(draw);
}

function stopWiredWaterfall() {
  if (wiredWaterfallRaf !== null) {
    cancelAnimationFrame(wiredWaterfallRaf);
    wiredWaterfallRaf = null;
  }
  if (wiredWaterfall) wiredWaterfall.reset();
}

/**
 * The currently relevant diagnostics + analyser + ctx for the Sound
 * help panel - whichever audio path the visitor most recently touched.
 * There is no single "the" audio context on this page (the wired demo
 * and the two-device endpoint can each exist independently), so the
 * panel always reflects the last one used rather than trying to merge
 * both into one report.
 */
let activeAudioSource = null; // { label, ctx, analyser, diagnostics }

function setActiveAudioSource(label, ctx, analyser, diagnostics) {
  activeAudioSource = { label, ctx, analyser, diagnostics };
  renderSoundHelp();
}

/**
 * Tells a visitor when audio did not actually start, rather than leaving
 * them looking at a demo that is visibly running but silent. Call once a
 * demo's own async setup has settled, so `ctx.state` reflects the real
 * outcome of its own resume() call rather than its still-pending promise.
 * Also covers WebKit's `interrupted` state - see wired.js's own doc on
 * https://bugs.webkit.org/show_bug.cgi?id=273511.
 *
 * This only catches the case `ctx.state` can actually see. A context
 * that is `running` and genuinely producing no sound - the ambient
 * session category respecting the iOS silent switch - reports nothing
 * here by design; see `checkAudibleOrWarn` below for the check that
 * covers that gap instead.
 */
function reportIfAudioSuspended(ctx) {
  if (ctx && (ctx.state === 'suspended' || ctx.state === 'interrupted')) {
    micDiagnostic.textContent = ctx.state === 'interrupted'
      ? 'Audio was interrupted (another app or a call likely took the audio session) - press the button again, or use Resume sound in Sound help below.'
      : 'Audio has not started for this tab yet - if this stays silent, check the device is not muted (the iOS silent switch mutes web audio too) and try the button again.';
    micDiagnostic.className = 'diagnostic diagnostic--warning';
    return true;
  }
  return false;
}

/**
 * The other half of the fix: a context can stay `running` throughout
 * and still never produce audible output on iOS (WebKit's ambient
 * session category respects the silent switch - see
 * audio-diagnostics.js). `ensurePlaybackAudioSession` is the real fix
 * for that; this is the honesty check on top of it, actually measuring
 * output via the analyser rather than trusting `ctx.state` alone, which
 * is exactly the case the old suspended-only warning always missed.
 */
async function checkAudibleOrWarn(ctx, analyser, diagnostics) {
  const heard = await waitForAudibleSignal(ctx, analyser);
  diagnostics.recordState(ctx);
  if (heard) {
    diagnostics.mark('rmsNonZero');
    renderSoundHelp();
    return true;
  }
  if (ctx && (ctx.state === 'suspended' || ctx.state === 'interrupted')) {
    // Already reported by reportIfAudioSuspended - nothing new to add.
    renderSoundHelp();
    return false;
  }
  micDiagnostic.textContent = 'Audio is running but no sound has been detected - check the mute switch and volume, then try Resume sound in Sound help below.';
  micDiagnostic.className = 'diagnostic diagnostic--warning';
  renderSoundHelp();
  return false;
}

// -----------------------------------------------------------------------
// Sound help - a visitor on an iPhone is the only person who can ever
// see whether this actually worked, and Dan is the only person who can
// test on one. This panel exists so the page can tell him what it knows
// rather than presenting a silent demo with nothing to go on.
// -----------------------------------------------------------------------
const CHECKPOINT_LABELS = {
  fetched: 'WASM fetched',
  moduleAdded: 'Worklet module loaded',
  wasmReady: 'WASM instantiated and ready',
  currentTimeAdvancing: 'Audio clock advancing',
  rmsNonZero: 'Output signal detected',
};

function renderSoundHelp() {
  soundHelpCheckpoints.innerHTML = '';
  if (!activeAudioSource) {
    const li = document.createElement('li');
    li.textContent = 'Nothing has tried to play audio yet - press Dial, or Enable microphone.';
    soundHelpCheckpoints.appendChild(li);
    soundHelpState.textContent = '';
    return;
  }
  const { label, ctx, analyser, diagnostics } = activeAudioSource;
  diagnostics.recordState(ctx);
  if (ctx && ctx.state === 'running' && analyser) {
    diagnostics.checkpoints.currentTimeAdvancing = ctx.currentTime > 0;
  }

  for (const [key, text] of Object.entries(CHECKPOINT_LABELS)) {
    const li = document.createElement('li');
    const done = diagnostics.checkpoints[key];
    li.textContent = `${done ? '[x]' : '[ ]'} ${text}`;
    soundHelpCheckpoints.appendChild(li);
  }

  const rms = analyser ? computeRms(analyser) : null;
  const stateLine = `${label}: context ${ctx ? ctx.state : 'not created'}${rms !== null ? `, output RMS ${rms.toFixed(4)}` : ''}${diagnostics.resumeError ? `, last resume() error: ${diagnostics.resumeError}` : ''}`;
  soundHelpState.textContent = stateLine;
}

resumeSoundBtn.addEventListener('click', () => {
  // A real click, unlike visibilitychange (see wired.js/modem.js's own
  // doc on why that alone is not a guaranteed user-activation signal for
  // WebKit's resume rules). Resumes everything that currently exists,
  // regardless of the state each thinks it is in - harmless to call
  // resume() on a context that is already running, and this button
  // exists specifically for the case where the automatic path did not
  // work and only a real gesture will.
  ensurePlaybackAudioSession();
  if (wired && wired.ctx) {
    wired.ctx.resume().catch((err) => {
      wired.diagnostics.resumeError = String(err && err.message ? err.message : err);
    });
  }
  if (endpoint && endpoint.ctx) {
    endpoint.ctx.resume().catch((err) => {
      endpoint.diagnostics.resumeError = String(err && err.message ? err.message : err);
    });
  }
  renderSoundHelp();
});

copyDiagnosticsBtn.addEventListener('click', async () => {
  const lines = [
    `modem.dbhq.uk diagnostics`,
    `captured: ${new Date().toISOString()}`,
    `page last-modified: ${document.lastModified}`,
    `user agent: ${navigator.userAgent}`,
    `secure context: ${window.isSecureContext}`,
    `audioSession: ${'audioSession' in navigator ? (navigator.audioSession.type || '(no type set)') : 'not exposed by this browser'}`,
  ];
  if (activeAudioSource) {
    const { label, ctx, analyser, diagnostics } = activeAudioSource;
    diagnostics.recordState(ctx);
    lines.push('', `active source: ${label}`, `context state: ${ctx ? ctx.state : 'none'}`, `currentTime: ${ctx ? ctx.currentTime.toFixed(3) : 'n/a'}`);
    if (analyser) lines.push(`output RMS: ${computeRms(analyser).toFixed(5)}`);
    lines.push(`checkpoints: ${JSON.stringify(diagnostics.checkpoints)}`);
    lines.push(`state history: ${diagnostics.stateHistory.map((s) => `${s.t}ms:${s.state}`).join(' -> ')}`);
    if (diagnostics.resumeError) lines.push(`last resume() error: ${diagnostics.resumeError}`);
    lines.push('events:', ...diagnostics.events.map((e) => `  ${e.t}ms  ${e.text}`));
  } else {
    lines.push('', 'no audio path has been used yet this session');
  }
  lines.push('', `CSP violations: ${cspViolations.length}`);
  for (const v of cspViolations) {
    lines.push(`  ${v.directive} blocked ${v.blockedURI} (${v.sourceFile}:${v.lineNumber})`);
  }

  const report = lines.join('\n');
  try {
    await navigator.clipboard.writeText(report);
    copyDiagnosticsStatus.textContent = 'Copied to clipboard.';
  } catch (err) {
    console.error('clipboard write failed', err);
    copyDiagnosticsStatus.textContent = 'Could not copy automatically - select and copy the text below.';
    const existing = copyDiagnosticsStatus.parentElement.querySelector('.sound-help__report');
    if (existing) existing.remove();
    const pre = document.createElement('pre');
    pre.className = 'sound-help__report';
    pre.textContent = report;
    copyDiagnosticsStatus.after(pre);
  }
});

// iOS suspends every AudioContext when the page is backgrounded, and
// does not resume it automatically when the visitor comes back - left
// alone, that is a demo that looks like it is still running but has
// gone silent. Re-resume whatever is currently live the moment the page
// is visible again, from inside the same visibilitychange handler that
// fires the moment a person switches back to the tab (itself a strong
// enough activation signal for WebKit's own resume rules, though not
// guaranteed - see the Resume sound button above for the explicit
// fallback). Also covers `interrupted`, not only `suspended`.
document.addEventListener('visibilitychange', () => {
  if (document.visibilityState !== 'visible') return;
  if (wired) wired.resumeIfSuspended();
  if (endpoint) endpoint.resumeIfSuspended();
  renderSoundHelp();
});

// -----------------------------------------------------------------------
// Routing: three routes behind one application - /demo, /originate,
// /receive - plus the plain landing (three buttons) at the root path,
// and #demo kept working as a legacy alias for /demo. Routes carry the
// role; nothing about the call itself (digits, connection phase) ever
// lives in the URL.
//
// The rule that shapes everything below: an internal tap on one of the
// three landing buttons starts that route immediately - clicking the
// button is itself the explicit gesture, so there is nothing left to
// confirm - while a direct link or a reload landing straight on a route
// must never play audio or ask for a microphone on its own. Both cases
// run through the exact same `startRoute`; what differs is only whether
// something else already supplied the gesture (a real click, handled
// synchronously inside the launcher buttons' own listeners) or whether
// this page has to ask for one first (`route-start-block`, populated by
// `showRouteStart` and only ever wired to call `startRoute` from inside
// its own click handler).
// -----------------------------------------------------------------------
function routeForLocation() {
  let path = location.pathname.replace(/\/index\.html$/, '');
  if (path.length > 1) path = path.replace(/\/+$/, '');
  if (path.endsWith('/demo')) return 'demo';
  if (path.endsWith('/originate')) return 'originate';
  if (path.endsWith('/receive')) return 'receive';
  // Legacy alias: a shared link to the old #demo hash on the plain root
  // path still has to work.
  if (location.hash === '#demo') return 'demo';
  return null;
}

const ROUTE_START_COPY = {
  demo: 'A direct link never plays audio on its own - press Start to hear both ends connect right here.',
  originate: 'This device will dial out - it needs your microphone, asked for only once you press Start.',
  receive: 'This device will listen for a call - it needs your microphone, asked for only once you press Start.',
};
const ROUTE_START_LABEL = {
  demo: 'Start demo',
  originate: 'Start originating modem',
  receive: 'Start receiving modem',
};

let pendingRoute = null;

function showLanding() {
  pendingRoute = null;
  // Undo whatever `launchRoute` did to a button that was pressed and then
  // failed to start, so the landing page is never left showing
  // "Starting..." on a dead control.
  for (const [button, label] of LAUNCH_LABELS) {
    button.disabled = false;
    button.textContent = label;
  }
  launcher.hidden = false;
  routeStartBlock.hidden = true;
  wiredPanel.hidden = true;
  endpointPanel.hidden = true;
}

function showRouteStart(route) {
  pendingRoute = route;
  launcher.hidden = true;
  routeStartBlock.hidden = false;
  routeStartCopy.textContent = ROUTE_START_COPY[route];
  routeStartBtn.textContent = ROUTE_START_LABEL[route];
  // Re-enabled as well as relabelled: a previous attempt that failed left
  // it disabled and reading "Starting...", and this is the one path back
  // to a usable button.
  routeStartBtn.disabled = false;
  wiredPanel.hidden = true;
  endpointPanel.hidden = true;
}

function initRouting() {
  const route = routeForLocation();
  if (route) {
    showRouteStart(route);
  } else {
    showLanding();
  }
}

async function startRoute(route) {
  if (route === 'demo') await startDemo();
  else if (route === 'originate') await startEndpointRoute(Role.ORIGINATE);
  else if (route === 'receive') await startEndpointRoute(Role.ANSWER);
}

// Trailing-slash form throughout: Cloudflare Pages 308s the slash-less
// path to this one (each route is a real directory - see
// .github/workflows/deploy.yml's own doc), which is also the form each
// route's own <link rel="canonical"> declares. Pushing that form
// directly means a visitor who reloads, shares, or bookmarks straight
// from the address bar never takes the redirect hop at all.
/** The three launcher buttons and their resting labels, captured once so
 * a pressed button can be put back exactly as it was if the route fails
 * to start. */
const LAUNCH_BUTTONS = [launchDemoBtn, launchOriginateBtn, launchReceiveBtn].filter(Boolean);
const LAUNCH_LABELS = new Map(LAUNCH_BUTTONS.map((b) => [b, b.textContent]));

/**
 * Starts a route from the landing page, and says so while it happens.
 *
 * Bringing a route up is not instant: the WASM has to be fetched, the
 * worklet module added, and the worklet side has to compile and
 * instantiate it - hundreds of milliseconds on a phone, and longer on a
 * cold connection. The old handler hid the launcher on the first line and
 * showed nothing at all until the panel was ready, so for that whole
 * stretch a tap produced an empty page. Indistinguishable, to the person
 * holding the phone, from a button that does not work - which is half of
 * what "the demo is flake on mobile" was describing (Dan, 10 Sep 2026);
 * the other half was the tap landing before the handlers existed, see
 * index.html.
 *
 * So the launcher stays put and the pressed button says what it is doing.
 * Nothing needs to hide it: every route ends in a full-viewport app-mode
 * panel (`position: fixed; inset: 0`) that covers it, and if the route
 * fails instead, `exitToLanding` -> `showLanding` restores the buttons to
 * exactly the labels captured above.
 */
function launchRoute(button, route, path) {
  history.pushState(null, '', path);
  for (const b of LAUNCH_BUTTONS) b.disabled = true;
  button.textContent = 'Starting...';
  startRoute(route);
}

launchDemoBtn.addEventListener('click', () => launchRoute(launchDemoBtn, 'demo', '/demo/'));
launchOriginateBtn.addEventListener('click', () => launchRoute(launchOriginateBtn, 'originate', '/originate/'));
launchReceiveBtn.addEventListener('click', () => launchRoute(launchReceiveBtn, 'receive', '/receive/'));

routeStartBtn.addEventListener('click', () => {
  if (!pendingRoute) return;
  const route = pendingRoute;
  // Same reasoning as `launchRoute`: say it is starting rather than
  // leaving an empty page behind while the WASM and the worklet load.
  routeStartBtn.disabled = true;
  routeStartBtn.textContent = 'Starting...';
  startRoute(route);
});

/** Back (either panel's own button) and Escape both fully exit to the
 * landing - there is no intermediate "in-page but not full-viewport"
 * state any more now that all three routes are dedicated full-viewport
 * views, so there is nothing to shrink back into. */
async function exitToLanding() {
  exitAppMode();
  if (wired) await teardownWired();
  if (endpoint) await teardownEndpoint();
  showLanding();
  if (location.pathname !== '/' || location.hash) {
    history.pushState(null, '', '/');
  }
}

appModeBackBtn.addEventListener('click', exitToLanding);
endpointBackBtn.addEventListener('click', exitToLanding);

document.addEventListener('keydown', (e) => {
  if (e.key === 'Escape' && document.body.classList.contains('app-mode')) exitToLanding();
});

// Back/forward navigation must not auto-start anything either - the same
// rule a fresh load follows. Tear down whatever was live and re-evaluate
// the new location exactly as a fresh load would.
window.addEventListener('popstate', async () => {
  exitAppMode();
  if (wired) await teardownWired();
  if (endpoint) await teardownEndpoint();
  initRouting();
});

// -----------------------------------------------------------------------
// app-mode - the full-viewport view every one of the three routes gets
// while live (Dan, 8 Sep 2026: "make the demo fill the phone screen on
// mobile nicely, almost become an app" - now every viewport size, and
// /originate and /receive's own view too, not the wired demo only). See
// style.css's own doc on why the shell rules target `.endpoint.app-mode`
// rather than `.wired.app-mode` specifically.
// -----------------------------------------------------------------------
let liveAppModePanel = null;

/**
 * Shows `panel` full-viewport and sizes it.
 *
 * Unhiding is done **here**, not by the caller, and the height is
 * measured after it. Both call sites used to do it themselves and they
 * did it in opposite orders: the two-device routes unhid the panel and
 * then entered app mode, while Demo entered app mode and unhid the panel
 * one line later - so Demo alone measured the viewport while its panel
 * was still `hidden` and while `body.app-mode`'s `overflow: hidden` had
 * only just been applied. On a phone that is exactly when the value is
 * least trustworthy: toggling body overflow moves the address bar, and
 * a `visualViewport.height` read synchronously in the same block is the
 * height from *before* the move. The panel then keeps that stale pixel
 * height, because `style.height` overrides `inset: 0`.
 *
 * That is a real candidate for "the demo is flaky on mobile when it
 * loads" (Dan, 10 Sep 2026): it is the Demo route that had the bad
 * ordering, and the symptom depends on where the address bar happened to
 * be, which is why it is intermittent rather than broken.
 *
 * So: unhide, measure, then measure again on the next frame once layout
 * and any address-bar movement have settled. The second call is
 * idempotent and costs one frame.
 */
function enterAppModeFor(panel) {
  liveAppModePanel = panel;
  document.body.classList.add('app-mode');
  panel.classList.add('app-mode');
  panel.hidden = false;
  updateAppModeViewportHeight();
  requestAnimationFrame(updateAppModeViewportHeight);
  const backBtn = panel.querySelector('.app-mode-back');
  if (backBtn) backBtn.focus();
}

function exitAppMode() {
  if (liveAppModePanel) {
    liveAppModePanel.classList.remove('app-mode');
    liveAppModePanel.style.removeProperty('height');
  }
  document.body.classList.remove('app-mode');
  liveAppModePanel = null;
}

/**
 * `100dvh` already tracks the browser's own address bar; it does not
 * reliably track the on-screen keyboard on every WebKit version. While
 * app mode is active, the visualViewport API (where present) is the
 * more honest source for "how much space is actually left above the
 * keyboard" - setting the panel's own height directly to it keeps the
 * composer visible above the keyboard rather than covered by it,
 * instead of a fixed-height box the keyboard simply overlaps.
 */
function updateAppModeViewportHeight() {
  if (!liveAppModePanel) return;
  const height = window.visualViewport?.height;
  // A zero or absent reading is not a viewport, it is a measurement
  // taken at the wrong moment - and writing it to `style.height` would
  // collapse the panel to nothing while `inset: 0` sat there ready to
  // have sized it correctly. Leaving the property alone falls back to
  // the stylesheet, which is the right answer whenever this is.
  if (!Number.isFinite(height) || height <= 0) return;
  liveAppModePanel.style.height = `${height}px`;
}

if (window.visualViewport) {
  window.visualViewport.addEventListener('resize', updateAppModeViewportHeight);
}

// -----------------------------------------------------------------------
// The automatic bidirectional data check - the actual success criterion
// for a real call in either mode (carrier detection alone is not
// success). A fixed, non-secret marker rather than anything a visitor
// could type - it is filtered out of both transcripts on arrival (see
// the 'data' listeners below), never shown as though it were real chat,
// and never printed as fabricated modem output either: it only ever
// drives this diagnostic line, outside the transcript.
// -----------------------------------------------------------------------
const CANARY = 'MODEM-CHECK-OK';
const DATA_CHECK_TIMEOUT_MS = 10000;

/**
 * Demo's own check: both real Sessions live in this one page, so the
 * joint verdict can be computed and shown directly. Originate already
 * holds the turn the instant both ends connect, so it sends first;
 * answer only ever sends its own canary back once its own `status` event
 * genuinely reports `hasTurn`, never on a fixed timer.
 *
 * `yieldTurn` is called a real ~800ms after `send`, not back to back.
 * The queueing order alone (`process_out`'s "yielding" bypass keeps
 * draining `tx` in order regardless of exactly when `hasTurn` itself
 * flips - see modem-core/src/session.rs's own doc) looked sufficient on
 * paper, but measured directly it was not: calling `yieldTurn`
 * immediately after `send` left the far end's `hasTurn` never once
 * observed true for the rest of the check window, even though the data
 * itself had already decoded correctly moments before - a real,
 * reproducible timing sensitivity around queueing a Turn packet with no
 * gap after other data, not a one-off flake. `modem-core`'s own test
 * suite never actually exercises this shape either: every hand-over test
 * there settles between operations (see `session.rs`'s own `settle`
 * helper), never queues a Turn packet immediately behind data with
 * nothing between them. A real settle here matches that established
 * convention rather than fighting it.
 */
function startWiredDataCheck() {
  let aReceivedB = false;
  let bReceivedA = false;
  let bSent = false;
  wiredDataCheck.textContent = 'Checking both directions carry real data...';
  wiredDataCheck.className = 'diagnostic';

  const onData = (e) => {
    const { side, bytes } = e.detail;
    const text = decodeWired(bytes);
    if (side === 'b' && text.includes(CANARY)) bReceivedA = true;
    if (side === 'a' && text.includes(CANARY)) aReceivedB = true;
    maybeFinish();
  };
  const onStatus = (e) => {
    const { b } = e.detail;
    if (!bSent && b.hasTurn && b.state === SessionState.CONNECTED) {
      bSent = true;
      wired.send('b', CANARY);
      // A real gap before yielding, not back-to-back - matching
      // modem-core's own test convention (every hand-over test settles
      // between operations, never queues a Turn packet immediately
      // behind data with no gap). Measured directly: yielding
      // immediately after send() here left the far end's `hasTurn`
      // never once observed true for the rest of the check window, even
      // though the data itself had already decoded correctly - a real,
      // reproducible timing sensitivity, not a one-off flake.
      setTimeout(() => wired.yieldTurn('b'), 800);
    }
  };

  function cleanup() {
    wired.removeEventListener('data', onData);
    wired.removeEventListener('status', onStatus);
    clearTimeout(timer);
  }
  function maybeFinish() {
    if (aReceivedB && bReceivedA) {
      wiredDataCheck.textContent = 'Bidirectional data check: passed - both ends received real bytes from the other.';
      wiredDataCheck.className = 'diagnostic';
      cleanup();
    }
  }
  const timer = setTimeout(() => {
    if (!(aReceivedB && bReceivedA)) {
      const missing = [];
      if (!bReceivedA) missing.push('originate to answer');
      if (!aReceivedB) missing.push('answer to originate');
      wiredDataCheck.textContent = `Bidirectional data check: failed - ${missing.join(' and ')} did not arrive within 10s.`;
      wiredDataCheck.className = 'diagnostic diagnostic--warning';
    }
    cleanup();
  }, DATA_CHECK_TIMEOUT_MS);

  wired.addEventListener('data', onData);
  wired.addEventListener('status', onStatus);
  wired.send('a', CANARY);
  setTimeout(() => wired.yieldTurn('a'), 800);
}

/**
 * The two-device check: this page only ever has one real Session, so it
 * can only honestly claim what it can measure - whether *this* end
 * received the far end's canary. That is still the real, load-bearing
 * proof for this end's own screen: with both pages open (the two-device
 * routes' own standing instruction), a visitor sees "received" appear on
 * both physical devices, together, which is what genuinely bidirectional
 * data looks like across two independent endpoints - never inferred from
 * one side alone. Same turn logic as the wired check, and it works
 * unmodified on both /originate and /receive: originate already holds
 * the turn at Connected and sends immediately; answer's own `status`
 * event only reports `hasTurn` once the real acoustic Turn packet has
 * actually arrived, and sends only then.
 */
function startEndpointDataCheck() {
  let received = false;
  let sentOwn = false;
  endpointDataCheck.textContent = 'Checking this end can send and receive real data...';
  endpointDataCheck.className = 'diagnostic';

  const onStatus = (e) => {
    if (!sentOwn && e.detail.hasTurn && e.detail.state === SessionState.CONNECTED) {
      sentOwn = true;
      endpoint.send(CANARY);
      // A real gap, not back-to-back - see startWiredDataCheck's own
      // doc for the measured reason this matters.
      setTimeout(() => endpoint.yieldTurn(), 800);
    }
  };
  const onData = (e) => {
    const text = new TextDecoder().decode(e.detail);
    if (text.includes(CANARY)) {
      received = true;
      endpointDataCheck.textContent = 'Bidirectional data check: this end received real bytes from the far end.';
      endpointDataCheck.className = 'diagnostic';
      cleanup();
    }
  };

  function cleanup() {
    endpoint.removeEventListener('status', onStatus);
    endpoint.removeEventListener('data', onData);
    clearTimeout(timer);
  }
  const timer = setTimeout(() => {
    if (!received) {
      endpointDataCheck.textContent = 'Bidirectional data check: nothing arrived from the far end within 10s - the acoustic link connected but data did not get through. A convincing-looking call is not the same as one that actually works.';
      endpointDataCheck.className = 'diagnostic diagnostic--warning';
    }
    cleanup();
  }, DATA_CHECK_TIMEOUT_MS);

  endpoint.addEventListener('status', onStatus);
  endpoint.addEventListener('data', onData);
}

// -----------------------------------------------------------------------
// /demo - two real Sessions cross-wired in software, right here in the
// page, the browser mirror of modem-audio/src/transport.rs's
// WiredTransport. No microphone and no permission prompt: both ends live
// in this page and the mixed signal plays through the visitor's own
// speakers.
//
// Answer auto-answers, matching the binary: `modem --single --acoustic
// --answer` answers at startup with nothing typed into it. There is no
// separate "ATA (answer)" control - `wired.answer()` runs as soon as the
// session pair exists.
// -----------------------------------------------------------------------
let wired = null;
let lastWiredA = null;
let lastWiredB = null;

function endpointStateName(state) {
  switch (state) {
    case SessionState.IDLE: return 'IDLE';
    case SessionState.DIALLING: return 'DIALLING';
    case SessionState.ANSWERING: return 'ANSWERING';
    case SessionState.CONNECTED: return 'CONNECTED';
    default: return String(state);
  }
}

/** The word on a panel's own status line, which is not always the name
 * of the `SessionState` behind it.
 *
 * `Session::answer()` moves straight to `ANSWERING` and stays there for
 * as long as the end sits off-hook with nothing on the line - which on
 * /receive is from the moment the page opens until a call actually
 * arrives, often for as long as somebody takes to pick up the other
 * device. Printing "ANSWERING" through all of that claims a call that
 * has not started (Dan, 10 Sep 2026: "Receive says answer before the
 * ring has even started - should it not say idle").
 *
 * It also contradicted the caption directly beneath it, which has always
 * read "Listening for a call" in exactly this state - see
 * `endpointCaptionFor`. Two lines, one above the other, disagreeing
 * about whether anything was happening.
 *
 * So the tally follows the carrier, not the enum: nothing on the line is
 * IDLE, a carrier the end has not finished bringing up is ANSWERING, and
 * `CONNECTED` speaks for itself. The state machine is untouched - this
 * is a labelling rule, and `Session` keeps reporting exactly what it
 * always did.
 */
function endpointStatusWord(state, carrier) {
  if (state === SessionState.ANSWERING && !carrier) return 'IDLE';
  return endpointStateName(state);
}

/** One word for a panel's own status line - the long combined string
 * (state/stage/turn/carrier) now lives only in the shared phase line
 * below, so the two are not saying almost the same thing twice.
 *
 * Uses the same carrier-led rule as the two-device panel (see
 * `endpointStatusWord`), and it shows up more sharply here: the answer
 * pane is created and answered the instant the demo starts, so it read
 * "ANSWERING" for the whole of the originating end's overture - eleven
 * seconds of dial tone, DTMF and ringback during which the answering end
 * has heard nothing at all, sitting beside a pane that is visibly still
 * dialling. */
function wiredShortStatus(status) {
  return endpointStatusWord(status.state, status.carrier);
}

/** The call's overall phase, shared above both panels rather than
 * repeated inside each - "both ends live" is one story, not two. */
function wiredPhaseLine(a, b) {
  if (a.state === SessionState.CONNECTED && b.state === SessionState.CONNECTED) {
    return 'CONNECTED';
  }
  if (a.state === SessionState.IDLE) {
    return 'IDLE - press ATDT to dial';
  }
  const stage = typeof a.stage === 'number' && a.stage >= 0 ? a.stage
    : typeof b.stage === 'number' && b.stage >= 0 ? b.stage
    : null;
  const stageName = stage !== null ? STAGE_NAMES[stage] : 'connecting';
  return `Handshake: ${stageName}`;
}

/**
 * Each panel's own honest gloss on the call's real phase - outside the
 * modem transcript on purpose (never the same element real modem output
 * lands in - see the top-level doc on never inventing modem output).
 * Sourced from the real overture/session state both ends already carry
 * in their own `status` payload: legitimate "shared call events" for
 * Demo specifically, because both ends are genuinely real and already in
 * this one page (not a stand-in for the acoustic observation the
 * two-device routes need instead - see their own captions below).
 * Originate's own caption never claims the far end is ringing, only ever
 * its own actual stage; answer's is driven by originate's real
 * stage/ring number precisely because nothing here is a timer guessing
 * at elapsed time.
 */
function wiredCaptionFor(side, a, b) {
  if (side === 'a') {
    if (a.state === SessionState.CONNECTED) return 'Connected';
    if (typeof a.stage === 'number' && a.stage === 3) return 'Calling - ringback (answers after 2 rings)';
    if (typeof a.stage === 'number' && a.stage >= 0) return 'Calling';
    return '';
  }
  if (b.state === SessionState.CONNECTED) return 'Connected';
  if (typeof a.stage !== 'number' || a.stage < 0) return 'Listening';
  if (a.stage < 3) return 'Listening';
  if (a.stage === 3) {
    return a.ringNumber ? `Incoming call - ring ${a.ringNumber} of 2` : 'Incoming call';
  }
  return 'Auto-answering';
}

function updateWiredComposerEnablement() {
  const bothConnected = !!lastWiredA && !!lastWiredB
    && lastWiredA.state === SessionState.CONNECTED
    && lastWiredB.state === SessionState.CONNECTED;
  wiredChat.disabled = !bothConnected;
  wiredSendBtn.disabled = !bothConnected;
}

/** Dials from originate with whatever is in the digits field - shared by
 * the automatic first dial (below) and the panel's own "ATDT (dial)"
 * redial button, so the two never drift into logging the command
 * differently. */
function dialWired() {
  const digits = wiredDigits.value || '0';
  wired.dial(digits);
  appendTerminalLine(wiredLogA, `ATDT${digits}`, { command: true });
}

let dialStarting = false;
let wiredDataCheckStarted = false;

async function startDemo() {
  if (dialStarting || wired) return;
  dialStarting = true;
  try {
    wired = new WiredEndpoint();
    await wired.init({ duplex: Duplex.HALF_PING_PONG });
    reportIfAudioSuspended(wired.ctx);
    setActiveAudioSource('demo', wired.ctx, wired.analyser, wired.diagnostics);
    wiredDataCheckStarted = false;

    wired.addEventListener('status', (e) => {
      const { a, b } = e.detail;
      lastWiredA = a;
      lastWiredB = b;
      wiredStatusA.textContent = wiredShortStatus(a);
      wiredStatusB.textContent = wiredShortStatus(b);
      wiredPhase.textContent = wiredPhaseLine(a, b);
      wiredCaptionA.textContent = wiredCaptionFor('a', a, b);
      wiredCaptionB.textContent = wiredCaptionFor('b', a, b);
      updateWiredComposerEnablement();
      wiredHangupBtn.disabled = a.state === SessionState.IDLE && b.state === SessionState.IDLE;
      if (!wiredDataCheckStarted && a.state === SessionState.CONNECTED && b.state === SessionState.CONNECTED) {
        wiredDataCheckStarted = true;
        startWiredDataCheck();
      }
      if (a.state === SessionState.IDLE && b.state === SessionState.IDLE) {
        wiredDataCheckStarted = false;
      }
    });

    wired.addEventListener('data', (e) => {
      const { side, bytes } = e.detail;
      const text = decodeWired(bytes);
      if (text.includes(CANARY)) return;
      appendTerminalLine(side === 'a' ? wiredLogA : wiredLogB, text);
    });

    wired.addEventListener('error', (e) => {
      appendTerminalLine(wiredLogA, `error: ${e.detail}`);
    });

    // Auto-answer - see this section's own doc above.
    wired.answer();

    startWiredWaterfall(wired);
    // `enterAppModeFor` unhides it - see its own doc on why the order
    // matters and why it is no longer the caller's to get wrong.
    enterAppModeFor(wiredPanel);
    // Auto-dial too - pressing Demo once is the entire demo, not the
    // first of several steps.
    dialWired();
    // Not awaited - a background honesty check (see checkAudibleOrWarn's
    // own doc), not something the dial flow itself should wait on.
    checkAudibleOrWarn(wired.ctx, wired.analyser, wired.diagnostics);
  } catch (err) {
    console.error(err);
    micDiagnostic.textContent = `Could not start the demo: ${err.message || err}`;
    micDiagnostic.className = 'diagnostic diagnostic--warning';
    await exitToLanding();
  } finally {
    dialStarting = false;
  }
}

/** Fully releases the audio graph rather than relying on hangup() alone -
 * see wired.js's own stop() doc: an idle Session outputs silence, but
 * the node itself keeps rendering it until the context closes. */
async function teardownWired() {
  if (!wired) return;
  await wired.stop();
  wired = null;
  lastWiredA = null;
  lastWiredB = null;
  stopWiredWaterfall();
  wiredPanel.hidden = true;
  wiredLogA.innerHTML = '';
  wiredLogB.innerHTML = '';
  wiredStatusA.textContent = 'IDLE';
  wiredStatusB.textContent = 'IDLE';
  wiredCaptionA.textContent = '';
  wiredCaptionB.textContent = '';
  wiredPhase.textContent = 'IDLE - press ATDT to dial';
  wiredDataCheck.textContent = '';
  updateWiredComposerEnablement();
  if (activeAudioSource && activeAudioSource.label === 'demo') {
    activeAudioSource = null;
    renderSoundHelp();
  }
}

wiredDialBtn.addEventListener('click', () => {
  if (wired) dialWired();
});

wiredHangupBtn.addEventListener('click', () => {
  if (wired) wired.hangup();
});

wiredStopBtn.addEventListener('click', teardownWired);

wiredSendBtn.addEventListener('click', () => {
  if (!wired || !wiredChat.value) return;
  const side = wiredSendSide.value === 'b' ? 'b' : 'a';
  wired.send(side, wiredChat.value);
  // Hands the turn back once this end is done - a chat composer that
  // never yielded left the far end with no way to ever reply (see
  // startWiredDataCheck's own doc on why this export exists at all). A
  // real gap before yielding, not back-to-back - see that same doc for
  // the measured reason a Turn packet queued with no gap after other
  // data left the far end's `hasTurn` never observed true.
  setTimeout(() => wired.yieldTurn(side), 800);
  appendTerminalLine(side === 'a' ? wiredLogA : wiredLogB, `> ${wiredChat.value}`);
  wiredChat.value = '';
});

wiredChat.addEventListener('keydown', (e) => {
  if (e.key === 'Enter') wiredSendBtn.click();
});

// -----------------------------------------------------------------------
// /originate and /receive - one real endpoint, a raw microphone, role
// fixed by the route rather than chosen on the page. Both share this one
// implementation, parameterised only by `role`: the turn-based
// bidirectional check above already works unmodified on either end (see
// its own doc), and everything else that differs between the two - the
// heading, the intro copy, the QR/share block, the caption wording - is
// looked up from `role` below rather than forked into two copies.
// -----------------------------------------------------------------------
const FIXED_DIGITS = '0000';
const ORIGINATE_NO_ANSWER_MS = 20000;
const RECEIVE_NOTHING_MS = 10000;
const RECEIVE_MOVE_CLOSER_MS = 30000;
/** Below this, an analyser reading is treated as room noise, not
 * microphone activity - not a calibrated voice-activity threshold, just
 * "is anything audible reaching this input at all". */
const MIC_ACTIVITY_RMS = 0.01;

let endpoint = null;
let endpointRole = null;
let endpointClockStop = null;
let lastEndpointState = null;
let lastEndpointCarrier = false;
let micActivityRaf = null;

function startMicActivityIndicator() {
  stopMicActivityIndicator();
  const poll = () => {
    if (!endpoint || !endpoint.analyser) return;
    const rms = computeRms(endpoint.analyser);
    micActivityDot.classList.toggle('signal-indicator__dot--active', rms !== null && rms > MIC_ACTIVITY_RMS);
    micActivityRaf = requestAnimationFrame(poll);
  };
  micActivityRaf = requestAnimationFrame(poll);
}

function stopMicActivityIndicator() {
  if (micActivityRaf !== null) {
    cancelAnimationFrame(micActivityRaf);
    micActivityRaf = null;
  }
  micActivityDot.classList.remove('signal-indicator__dot--active');
}

/**
 * Runs `onTick(elapsedMs, running)` about twice a second for as long as
 * this route is live, accumulating elapsed time only while `ctx` reports
 * `running` - paused, not merely slowed, while suspended or interrupted,
 * so a deadline never counts down time the audio was not actually able
 * to use (Dan's brief: "Deadlines, counted only while audio is actually
 * running"). Returns a stop function.
 */
function startRunningClock(ctx, onTick) {
  let elapsedMs = 0;
  let last = performance.now();
  const id = setInterval(() => {
    const now = performance.now();
    const dt = now - last;
    last = now;
    const running = !!ctx && ctx.state === 'running';
    if (running) elapsedMs += dt;
    onTick(elapsedMs, running);
  }, 500);
  return () => clearInterval(id);
}

/** Never claims a detection that never happened - originate only ever
 * reports its own actual stage ("Calling", never "ringing" - it cannot
 * know that from its own ringback), answer only its own real state and
 * the real carrier flag. */
function endpointCaptionFor(role, state, stage, carrier) {
  if (role === Role.ORIGINATE) {
    if (state === SessionState.CONNECTED) return 'Connected';
    if (state === SessionState.DIALLING && typeof stage === 'number' && stage === 3) {
      return 'Calling - ringback (answers after 2 rings)';
    }
    if (state === SessionState.DIALLING && typeof stage === 'number' && stage >= 0) return 'Calling';
    return '';
  }
  if (state === SessionState.CONNECTED) return 'Connected';
  if (state === SessionState.ANSWERING) {
    return carrier ? 'Modem signal detected - connecting' : 'Listening for a call';
  }
  return '';
}

function updateEndpointDeadline(role, elapsedMs) {
  if (role === Role.ORIGINATE) {
    if (lastEndpointState === SessionState.DIALLING && elapsedMs > ORIGINATE_NO_ANSWER_MS) {
      endpointDeadline.textContent = 'No answer within 20s - ending this attempt.';
      endpointDeadline.className = 'diagnostic diagnostic--warning';
      if (endpoint) endpoint.hangup();
    }
    return;
  }
  if (lastEndpointState === SessionState.ANSWERING && !lastEndpointCarrier) {
    if (elapsedMs > RECEIVE_MOVE_CLOSER_MS) {
      endpointDeadline.textContent = 'Still nothing after 30s - try moving the devices closer (10-20cm), somewhere quieter, or use Demo instead.';
      endpointDeadline.className = 'diagnostic diagnostic--warning';
    } else if (elapsedMs > RECEIVE_NOTHING_MS) {
      endpointDeadline.textContent = 'Nothing recognised yet - make sure the originating device has started calling too.';
      endpointDeadline.className = 'diagnostic';
    }
  } else {
    endpointDeadline.textContent = '';
  }
}

function renderShareBlock(url) {
  qrCodeEl.innerHTML = '';
  if (window.qrcode) {
    try {
      const qr = window.qrcode(0, 'M');
      qr.addData(url);
      qr.make();
      qrCodeEl.innerHTML = qr.createSvgTag({ cellSize: 4, margin: 4, scalable: true });
    } catch (err) {
      console.error('QR generation failed', err);
    }
  }
  shareLinkInput.value = url;
  shareCopyStatus.textContent = '';
}

shareCopyBtn.addEventListener('click', async () => {
  try {
    await navigator.clipboard.writeText(shareLinkInput.value);
    shareCopyStatus.textContent = 'Copied to clipboard.';
  } catch (err) {
    console.error(err);
    shareLinkInput.select();
    shareCopyStatus.textContent = 'Could not copy automatically - select and copy the text above.';
  }
});

async function startEndpointRoute(role) {
  endpointRole = role;
  lastEndpointState = null;
  lastEndpointCarrier = false;
  endpointHeading.textContent = role === Role.ORIGINATE ? 'Originate - dialling out' : 'Receive - listening for a call';
  endpointIntro.textContent = role === Role.ORIGINATE
    ? 'This device dials out. There is no real telephone network behind this, so the digits are fixed and decorative - only the modem handshake and the connection underneath it are real.'
    : 'This device listens for a call and answers automatically - nothing to type.';
  wiredPanel.hidden = true;
  enterAppModeFor(endpointPanel);
  endpointStatus.textContent = 'Waiting for microphone permission';
  endpointCaption.textContent = '';
  endpointCaption.className = 'endpoint__caption';
  endpointDeadline.textContent = '';
  endpointDataCheck.textContent = '';
  signalIndicators.hidden = true;
  chatLog.innerHTML = '';

  if (role === Role.ORIGINATE) {
    shareBlock.hidden = false;
    renderShareBlock(`${location.origin}/receive/`);
  } else {
    shareBlock.hidden = true;
  }

  let dataCheckStarted = false;
  try {
    endpoint = new ModemEndpoint();
    await endpoint.init({ role, duplex: Duplex.HALF_PING_PONG });

    endpoint.addEventListener('status', (e) => {
      const { state, stage, carrier } = e.detail;
      lastEndpointState = state;
      lastEndpointCarrier = carrier;
      endpointStatus.textContent = endpointStatusWord(state, carrier);
      endpointCaption.textContent = endpointCaptionFor(role, state, stage, carrier);
      modemSignalDot.classList.toggle('signal-indicator__dot--active', carrier);
      const connected = state === SessionState.CONNECTED;
      chatInput.disabled = !connected;
      chatSendBtn.disabled = !connected;
      endpointHangupBtn.disabled = state === SessionState.IDLE;
      if (connected && !dataCheckStarted) {
        dataCheckStarted = true;
        startEndpointDataCheck();
      }
      if (state === SessionState.IDLE) {
        dataCheckStarted = false;
        endpointDataCheck.textContent = '';
      }
    });

    endpoint.addEventListener('data', (e) => {
      const text = new TextDecoder().decode(e.detail);
      if (text.includes(CANARY)) return;
      appendTerminalLine(chatLog, text);
    });

    endpoint.addEventListener('error', (e) => {
      appendTerminalLine(chatLog, `error: ${e.detail}`);
    });

    const diagnostics = await endpoint.openMicrophone();
    const summary = summariseMicDiagnostics(diagnostics);
    micDiagnostic.textContent = summary;
    micDiagnostic.className = diagnostics.warnings.length > 0 ? 'diagnostic diagnostic--warning' : 'diagnostic';
    reportIfAudioSuspended(endpoint.ctx);
    setActiveAudioSource('endpoint', endpoint.ctx, endpoint.analyser, endpoint.diagnostics);

    signalIndicators.hidden = false;
    startMicActivityIndicator();

    // Only from here - the microphone is actually capturing - does the
    // distance guidance belong on screen for /receive (Dan's brief: "once
    // actually capturing", not before).
    if (role === Role.ANSWER) {
      endpointIntro.textContent = 'Listening for a call. Put the two devices roughly 10-20cm apart, speakers and microphones uncovered, both pages open - a starting point to validate on real hardware, not a promise.';
    }

    endpointClockStop = startRunningClock(endpoint.ctx, (elapsedMs, running) => {
      if (!running) {
        endpointDeadline.textContent = 'Audio is suspended - tap to resume (see Sound help below).';
        endpointDeadline.className = 'diagnostic diagnostic--warning';
        return;
      }
      updateEndpointDeadline(role, elapsedMs);
    });

    if (role === Role.ORIGINATE) {
      endpoint.dial(FIXED_DIGITS);
      appendTerminalLine(chatLog, `ATDT${FIXED_DIGITS}`, { command: true });
    } else {
      endpoint.answer();
    }
    endpointHangupBtn.disabled = false;

    checkAudibleOrWarn(endpoint.ctx, endpoint.analyser, endpoint.diagnostics);
  } catch (err) {
    console.error(err);
    endpointStatus.textContent = 'IDLE';
    if (err && (err.name === 'NotAllowedError' || err.name === 'PermissionDeniedError')) {
      endpointCaption.textContent = 'Microphone permission was denied - allow access and press Start again.';
    } else {
      endpointCaption.textContent = `Could not start: ${err.message || err}`;
    }
    endpointCaption.className = 'endpoint__caption diagnostic--warning';
    await exitToLanding();
  }
}

/** The one control that fully releases the microphone rather than just
 * ending a call - spike/README.md finding 5's rule applies to more than
 * a forgotten carrier: a visitor should never have to close the tab to
 * know their microphone is off. */
async function teardownEndpoint() {
  stopMicActivityIndicator();
  if (endpointClockStop) {
    endpointClockStop();
    endpointClockStop = null;
  }
  if (endpoint) {
    await endpoint.stop();
  }
  endpoint = null;
  endpointRole = null;
  lastEndpointState = null;
  lastEndpointCarrier = false;
  endpointPanel.hidden = true;
  chatLog.innerHTML = '';
  endpointStatus.textContent = 'IDLE';
  endpointCaption.textContent = '';
  endpointDeadline.textContent = '';
  endpointDataCheck.textContent = '';
  signalIndicators.hidden = true;
  shareBlock.hidden = true;
  if (activeAudioSource && activeAudioSource.label === 'endpoint') {
    activeAudioSource = null;
    renderSoundHelp();
  }
}

endpointHangupBtn.addEventListener('click', () => {
  if (endpoint) endpoint.hangup();
});

endpointStopBtn.addEventListener('click', teardownEndpoint);

chatSendBtn.addEventListener('click', () => {
  if (!endpoint || !chatInput.value) return;
  endpoint.send(chatInput.value);
  // A real gap before yielding - see startWiredDataCheck's own doc for
  // the measured reason a Turn packet queued with no gap after other
  // data left the far end's `hasTurn` never observed true.
  setTimeout(() => endpoint.yieldTurn(), 800);
  appendTerminalLine(chatLog, `> ${chatInput.value}`);
  chatInput.value = '';
});

chatInput.addEventListener('keydown', (e) => {
  if (e.key === 'Enter') chatSendBtn.click();
});

// A page navigating away must not leave a live microphone, a live wired
// call, or a carrier running - see spike/README.md finding 5 and both
// endpoints' own `stop()` docs. Best-effort: `beforeunload` cannot
// await, but stop() only tears down local objects (tracks, node,
// context), none of which need a network round trip.
window.addEventListener('beforeunload', () => {
  if (endpoint) endpoint.stop();
  if (wired) wired.stop();
});

// Populates the "nothing tried yet" state rather than leaving the panel
// empty until the first click - a visitor who opens Sound help before
// pressing anything should see that reflected accurately, not a blank list.
renderSoundHelp();

// The three launcher buttons ship `disabled` and are enabled here, at the
// very end of this module, once every handler above is attached. See
// index.html's own comment for the race this closes: they used to be
// tappable from the moment the HTML painted, while this module and the
// six it imports were still arriving, so an early tap on a cold mobile
// connection hit nothing at all.
//
// Last statement before `initRouting`, and deliberately not earlier:
// anything between this line and the listeners would be a window where
// the button works only partly. Both run in the same task, so no frame is
// ever painted with the launcher visible and its buttons dead.
//
// `route-start-block` needs no equivalent - it ships `hidden` and only
// `showRouteStart` reveals it, so it cannot be pressed before this module
// runs in the first place.
for (const btn of [launchDemoBtn, launchOriginateBtn, launchReceiveBtn]) {
  if (btn) btn.disabled = false;
}

initRouting();
