// explained.html's own behaviour - the overture playback and the
// phase-by-phase explainer, split out of page.js when the nine phases
// moved to their own URL (see explained.html's own doc, and the design
// spec's defect 1). Extracted rather than shared with page.js: this
// page's DOM has none of index.html's demo/wired/endpoint elements, and
// page.js's top-level `document.getElementById` calls for those would
// throw the moment this module loaded on a page that does not have them.
import { instantiateModemModule, SessionHandle, Role, Duplex, SessionState, STAGE_NAMES } from './session.js';
import { ensurePlaybackAudioSession, attachSecurityPolicyListener, AudioDiagnostics } from './audio-diagnostics.js';
import { Waterfall } from './waterfall.js';
import { appendTerminalLine } from './terminal-line.js';

// See page.js's identical call for why this runs this early and is safe
// to call from more than one page.
ensurePlaybackAudioSession();
attachSecurityPolicyListener();

const terminalOutput = document.getElementById('terminal-output');
const nowPlaying = document.getElementById('now-playing');
const overtureBtn = document.getElementById('overture-btn');
const canvas = document.getElementById('waterfall');
const phaseListEl = document.getElementById('phase-list');
const audioDiagnostic = document.getElementById('explained-audio-diagnostic');

const waterfall = new Waterfall(canvas);

// -----------------------------------------------------------------------
// The nine phase rows themselves are no longer built here. Before this
// split, this file's ancestor (page.js) held a hardcoded PHASES array
// (label + description) and used it to *generate* the <li> markup at
// runtime - which is exactly why the nine phases were invisible to a
// crawler: the initial HTML response contained an empty <ol>, and only a
// JavaScript-executing client ever saw the content (design spec, defect
// 1).
//
// The fix chosen here is to read the array back out of the DOM rather
// than generate the DOM from an array: explained.html now carries the
// full markup for each phase - id, data-stage, the STAGE_NAMES label and
// its description - as static content, and this module's only job is to
// wire a click handler onto the "Play" button already sitting inside
// each one. There is now exactly one copy of the phase text, in the
// HTML, and it is the copy a crawler sees on the first response with no
// script required. (The alternative the design spec offered - generate
// the HTML from the array at build time - would have needed a new build
// step in a repo that otherwise only generates the two terminal frames
// via frames.py; reading the DOM back needs no build step at all, and
// removes a whole array from this file instead of adding one.)
//
// data-stage is trusted as the source of truth for indexing into
// STAGE_NAMES/session playback; the row's own <h3> text is never
// re-parsed to find it, so a heading's wording can change freely without
// touching this logic.
const phaseRows = Array.from(phaseListEl.querySelectorAll('.phase-row'));

// -----------------------------------------------------------------------
// Prerendering the overture: the same modem-wasm build the real endpoint
// uses, run once on the main thread (no AudioWorklet needed for this -
// nothing here is on a real-time deadline) to render the whole
// performance into one buffer, segmented by the stage that owns each
// sample. Playing back a slice of that one real rendering is what makes
// "each phase individually playable" actually play the genuine DSP
// output rather than a second, hand-rolled tone generator that could
// drift from it.
// -----------------------------------------------------------------------
let audioCtx = null;
let prerenderPromise = null;
const overtureDiagnostics = new AudioDiagnostics('overture');

// Safari (desktop and every iOS browser - Apple requires all of them to
// run WebKit) still ships this prefixed on some versions.
const AudioContextCtor = window.AudioContext || window.webkitAudioContext;

function ensureAudioContext() {
  if (!audioCtx) {
    if (!AudioContextCtor) {
      throw new Error('this browser exposes no AudioContext (or webkitAudioContext) at all');
    }
    ensurePlaybackAudioSession();
    audioCtx = new AudioContextCtor();
    overtureDiagnostics.recordState(audioCtx);
  }
  if (audioCtx.state === 'suspended' || audioCtx.state === 'interrupted') {
    // Fired synchronously here, on first use - see this function's own
    // callers, every one of which is the first statement evaluated in a
    // click handler, before any await. WebKit only unlocks a context
    // when resume() is called inside that synchronous window; call it
    // one tick later (after even a single `await`) and it stays
    // suspended forever with no error.
    audioCtx.resume().catch((err) => {
      overtureDiagnostics.resumeError = String(err && err.message ? err.message : err);
      overtureDiagnostics.log(`resume() rejected: ${overtureDiagnostics.resumeError}`);
    });
  }
  return audioCtx;
}

/**
 * Tells a visitor when audio did not actually start, rather than leaving
 * them looking at a demo that is visibly running but silent - the same
 * check page.js runs for the wired/endpoint audio paths, kept as its own
 * small copy here rather than imported: this page has one diagnostic
 * line, not the full Sound help panel index.html carries for its harder
 * cases (a live microphone, real hardware), so there is no shared
 * `renderSoundHelp` for this to hook into.
 */
function reportIfAudioSuspended(ctx) {
  if (ctx && (ctx.state === 'suspended' || ctx.state === 'interrupted')) {
    audioDiagnostic.textContent = ctx.state === 'interrupted'
      ? 'Audio was interrupted (another app or a call likely took the audio session) - press play again.'
      : 'Audio has not started for this tab yet - if this stays silent, check the device is not muted (the iOS silent switch mutes web audio too) and try again.';
    audioDiagnostic.className = 'diagnostic diagnostic--warning';
    return true;
  }
  return false;
}

// Same iOS backgrounding fix as page.js's identical handler: a context
// suspended by the tab going to the background does not resume itself
// when the visitor comes back.
document.addEventListener('visibilitychange', () => {
  if (document.visibilityState !== 'visible') return;
  if (audioCtx && (audioCtx.state === 'suspended' || audioCtx.state === 'interrupted')) {
    audioCtx.resume().catch((err) => {
      overtureDiagnostics.resumeError = String(err && err.message ? err.message : err);
    });
  }
});

async function prerenderOverture(ctx) {
  const sampleRate = 8000; // modem-core's own internal rate - no resampling
  const response = await fetch('/modem.wasm');
  const bytes = await response.arrayBuffer();
  const exportsHandle = await instantiateModemModule(bytes);
  const session = new SessionHandle(exportsHandle, { sampleRate, role: Role.ORIGINATE, duplex: Duplex.HALF_PING_PONG });
  session.dial(new TextEncoder().encode('01234567890'));

  const BLOCK = 256;
  const CONNECTED_TAIL_S = 1.2; // enough to hear CONNECT 300 land, no longer
  const SAFETY_CAP_S = 20; // the overture's own documented upper bound

  const chunks = [];
  const segments = [];
  let cursor = 0;
  let current = null;
  let connectedAt = null;
  const buf = new Float32Array(BLOCK);

  for (;;) {
    session.processOut(buf);
    chunks.push(buf.slice());
    const state = session.state();
    const stage = session.stage();
    const label = state === SessionState.CONNECTED ? 'connected' : stage;

    if (current === null || current.label !== label) {
      if (current) current.endSample = cursor;
      current = { label, startSample: cursor, endSample: null };
      segments.push(current);
    }
    cursor += BLOCK;

    if (state === SessionState.CONNECTED) {
      if (connectedAt === null) connectedAt = cursor;
      if ((cursor - connectedAt) / sampleRate >= CONNECTED_TAIL_S) break;
    }
    if (cursor / sampleRate > SAFETY_CAP_S) break;
  }
  current.endSample = cursor;
  session.free();

  const master = new Float32Array(cursor);
  let offset = 0;
  for (const chunk of chunks) {
    master.set(chunk, offset);
    offset += chunk.length;
  }

  const audioBuffer = ctx.createBuffer(1, master.length, sampleRate);
  audioBuffer.copyToChannel(master, 0);

  const stageSegments = new Map();
  for (const seg of segments) {
    if (typeof seg.label === 'number') stageSegments.set(seg.label, seg);
  }

  return { audioBuffer, segments, stageSegments, sampleRate };
}

function ensurePrerendered() {
  const ctx = ensureAudioContext();
  if (!prerenderPromise) {
    prerenderPromise = prerenderOverture(ctx);
  }
  return prerenderPromise;
}

let activeTimers = [];
function clearScheduledAnnotations() {
  for (const t of activeTimers) clearTimeout(t);
  activeTimers = [];
}

function labelText(label) {
  return label === 'connected' ? 'CONNECT 300' : STAGE_NAMES[label];
}

function setActivePhaseRow(label) {
  phaseRows.forEach((row) => row.classList.remove('phase-row--active'));
  if (typeof label === 'number') {
    const row = phaseListEl.querySelector(`[data-stage="${label}"]`);
    if (row) row.classList.add('phase-row--active');
  }
}

// -----------------------------------------------------------------------
// Exactly one clip plays at a time - the Dial button's full overture and
// every phase-row's own slice all go through `playSlice`, and starting
// any one of them stops whatever else was running first.
// -----------------------------------------------------------------------
let currentPlayback = null;

function stopCurrentPlayback() {
  if (currentPlayback) {
    const playing = currentPlayback;
    currentPlayback = null;
    playing.stop();
  }
}

/** Plays `[startSample, endSample)` of the prerendered buffer through the
 * waterfall's analyser, resolving once playback ends (naturally, or via
 * `stopCurrentPlayback`). Pre-empts whatever else was already playing. */
function playSlice({ audioBuffer, sampleRate }, startSample, endSample) {
  stopCurrentPlayback();

  const ctx = ensureAudioContext();
  const analyser = ctx.createAnalyser();
  analyser.fftSize = 2048;
  const source = ctx.createBufferSource();
  source.buffer = audioBuffer;
  source.connect(analyser);
  analyser.connect(ctx.destination);

  let raf = null;
  const draw = () => {
    waterfall.frame(analyser, ctx.sampleRate);
    raf = requestAnimationFrame(draw);
  };
  raf = requestAnimationFrame(draw);

  const offset = startSample / sampleRate;
  const duration = (endSample - startSample) / sampleRate;
  source.start(0, offset, duration);

  const token = {
    stop() {
      try {
        source.stop();
      } catch (_err) {
        // Already stopped or never started - nothing left to do.
      }
    },
  };
  currentPlayback = token;

  return new Promise((resolve) => {
    source.onended = () => {
      cancelAnimationFrame(raf);
      if (currentPlayback === token) currentPlayback = null;
      resolve();
    };
  });
}

async function playFullOverture() {
  overtureBtn.disabled = true;
  resetAllPhaseButtons();
  nowPlaying.textContent = 'Loading...';
  terminalOutput.innerHTML = '';
  waterfall.reset();

  const rendered = await ensurePrerendered();
  reportIfAudioSuspended(audioCtx);
  appendTerminalLine(terminalOutput, 'ATDT01234567890', { command: true });

  clearScheduledAnnotations();
  for (const seg of rendered.segments) {
    const atMs = (seg.startSample / rendered.sampleRate) * 1000;
    activeTimers.push(setTimeout(() => {
      nowPlaying.textContent = labelText(seg.label);
      setActivePhaseRow(seg.label);
      if (seg.label === 'connected') {
        appendTerminalLine(terminalOutput, 'CONNECT 300', { command: true });
      } else {
        appendTerminalLine(terminalOutput, STAGE_NAMES[seg.label]);
      }
    }, atMs));
  }

  await playSlice(rendered, 0, rendered.segments[rendered.segments.length - 1].endSample);

  clearScheduledAnnotations();
  nowPlaying.textContent = 'Idle';
  setActivePhaseRow(null);
  overtureBtn.disabled = false;
}

overtureBtn.addEventListener('click', () => {
  playFullOverture().catch((err) => {
    console.error(err);
    nowPlaying.textContent = 'Playback failed - press play to try again';
    overtureBtn.disabled = false;
  });
});

// -----------------------------------------------------------------------
// The explainer: one phase-row per stage, each playing its own slice of
// the identical rendering the dial button uses. Exactly one plays at a
// time (see `currentPlayback` above) - clicking a row's button while it
// is the one playing stops it early; clicking a different row's button
// switches to that clip instead of layering on top of the first.
// -----------------------------------------------------------------------
let activePhaseIndex = null;

function resetAllPhaseButtons() {
  phaseListEl.querySelectorAll('.phase-row__play').forEach((b) => {
    b.textContent = 'Play';
  });
  activePhaseIndex = null;
}

phaseRows.forEach((row) => {
  const index = Number(row.dataset.stage);
  const button = row.querySelector('.phase-row__play');

  button.addEventListener('click', async () => {
    if (activePhaseIndex === index) {
      // Already the one playing - a second click stops it, and does not
      // start anything new.
      stopCurrentPlayback();
      return;
    }

    resetAllPhaseButtons();
    activePhaseIndex = index;
    button.textContent = 'Stop';
    try {
      const rendered = await ensurePrerendered();
      const seg = rendered.stageSegments.get(index);
      if (!seg) throw new Error(`stage ${index} did not appear in the rendered overture`);
      nowPlaying.textContent = STAGE_NAMES[index];
      setActivePhaseRow(index);
      await playSlice(rendered, seg.startSample, seg.endSample);
    } catch (err) {
      console.error(err);
    } finally {
      // Guards against a preempted clip's own cleanup running after a
      // newer one has already taken over - see playSlice's token
      // handling, which is the other half of this same guard.
      if (activePhaseIndex === index) {
        nowPlaying.textContent = 'Idle';
        setActivePhaseRow(null);
        button.textContent = 'Play';
        activePhaseIndex = null;
      }
    }
  });
});

// A direct link to one phase (e.g. shared as
// https://modem.dbhq.uk/explained#phase-ansam) needs no help landing on
// it any more: the nine <li> elements are real, static markup in this
// page's initial HTML (not built by a loop after the fact, as they were
// on index.html before this page existed - see this file's own doc
// above), and style.css's own scroll-margin-top on .phase-row keeps the
// sticky nav from covering the target once the browser's native
// fragment scroll lands on it. Nothing left for this module to do here.
