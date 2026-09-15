// The device end of the acoustic lab. Loads on a phone and a laptop in
// the same room, registers, polls for an instruction, carries it out,
// and posts the result back. The operator writes the instructions into
// KV from their own machine; see web/_worker.js for the collector.
//
// # Why this is not web/calibrate.html
//
// /calibrate is the standalone instrument: a human presses buttons and
// copies JSON out. That works and needs nothing but a browser. This is
// the version with a wire back, because the interesting measurements
// need *both* devices doing specific things at the same moment - one
// holding a tone while the other listens - and coordinating that by
// shouting across a room is how you get measurements you cannot trust.
//
// # The `modem` op matters most
//
// The tone and measure ops characterise the channel. The `modem` op runs
// the real ModemEndpoint - the same worklet, the same WASM, the same
// code path the site uses - and streams its status, diagnostics and
// decoded bytes back here. So a failing two-device call can be watched
// from both ends at once with timestamps, rather than reconstructed
// afterwards from what someone saw on a screen.

import { ModemEndpoint, summariseMicDiagnostics } from './modem.js';
import { Role, Duplex, SessionState } from './session.js';

const TONES = { space1070: 1070, mark1270: 1270, space2025: 2025, mark2225: 2225 };
/// Off-band, so a level can be read against this room's own floor rather
/// than an absolute that means nothing across different hardware.
const REFERENCE_HZ = 3000;
const POLL_MS = 1000;

const params = new URLSearchParams(location.search);
const device = (params.get('device') || localStorage.getItem('lab-device') || `dev-${Math.random().toString(36).slice(2, 7)}`)
  .replace(/[^\w.-]/g, '')
  .slice(0, 40);
localStorage.setItem('lab-device', device);

const els = {
  who: document.getElementById('who'),
  state: document.getElementById('state'),
  log: document.getElementById('log'),
  join: document.getElementById('join'),
  stop: document.getElementById('stop'),
};
els.who.textContent = `device: ${device}`;

function log(line) {
  const at = new Date().toISOString().slice(11, 23);
  els.log.textContent = `${at}  ${line}\n${els.log.textContent}`.slice(0, 20000);
}
const setState = (t) => { els.state.textContent = t; };

// Carried on every write. Taken from the URL the device was opened with
// and kept, so a reload does not silently drop it - see _worker.js on
// why this is a speed bump rather than authentication.
const labKey = params.get('key') || localStorage.getItem('lab-key') || '';
if (labKey) localStorage.setItem('lab-key', labKey);

async function post(path, body) {
  try {
    const qs = labKey ? `?key=${encodeURIComponent(labKey)}` : '';
    const res = await fetch(`/api/lab/${path}${qs}`, {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ device, ...body }),
    });
    return await res.json();
  } catch (err) {
    log(`post ${path} failed: ${err}`);
    return null;
  }
}

const report = (kind, data) => post('report', { kind, data });

// ---------------------------------------------------------------------
// Audio
// ---------------------------------------------------------------------
let ctx = null;
let analyser = null;
let micStream = null;

function goertzel(samples, freq, rate) {
  const k = (2 * Math.PI * freq) / rate;
  const coeff = 2 * Math.cos(k);
  let s1 = 0;
  let s2 = 0;
  for (let i = 0; i < samples.length; i++) {
    const s0 = samples[i] + coeff * s1 - s2;
    s2 = s1;
    s1 = s0;
  }
  return (2 * Math.hypot(s1 - s2 * Math.cos(k), s2 * Math.sin(k))) / samples.length;
}

async function ensureAudio() {
  if (analyser) return;
  const AudioCtor = window.AudioContext || window.webkitAudioContext;
  try {
    // play-and-record, not playback: on iOS `playback` is output-only
    // and getUserMedia is refused outright while it is set.
    if ('audioSession' in navigator) navigator.audioSession.type = 'play-and-record';
  } catch { /* not WebKit */ }
  ctx = new AudioCtor();
  await ctx.resume();

  let exact = true;
  try {
    micStream = await navigator.mediaDevices.getUserMedia({
      audio: {
        echoCancellation: { exact: false },
        noiseSuppression: { exact: false },
        autoGainControl: { exact: false },
      },
    });
  } catch {
    exact = false;
    micStream = await navigator.mediaDevices.getUserMedia({
      audio: { echoCancellation: false, noiseSuppression: false, autoGainControl: false },
    });
  }
  const track = micStream.getAudioTracks()[0];
  const settings = track.getSettings ? track.getSettings() : {};
  analyser = ctx.createAnalyser();
  analyser.fftSize = 2048;
  ctx.createMediaStreamSource(micStream).connect(analyser);

  await post('hello', {
    userAgent: navigator.userAgent,
    contextSampleRate: ctx.sampleRate,
    audioSession: 'audioSession' in navigator ? navigator.audioSession.type : null,
    microphone: {
      requestedExact: exact,
      echoCancellation: settings.echoCancellation ?? null,
      noiseSuppression: settings.noiseSuppression ?? null,
      autoGainControl: settings.autoGainControl ?? null,
      label: track.label || null,
      sampleRate: settings.sampleRate ?? null,
    },
  });
  log(`audio up at ${ctx.sampleRate} Hz`);
}

async function measure(seconds) {
  const buf = new Float32Array(analyser.fftSize);
  const acc = {};
  const names = [...Object.keys(TONES), 'reference', 'rms'];
  for (const n of names) acc[n] = { sum: 0, peak: 0, n: 0 };
  const until = performance.now() + seconds * 1000;
  while (performance.now() < until) {
    analyser.getFloatTimeDomainData(buf);
    for (const [name, hz] of Object.entries(TONES)) {
      const v = goertzel(buf, hz, ctx.sampleRate);
      acc[name].sum += v; acc[name].peak = Math.max(acc[name].peak, v); acc[name].n++;
    }
    const r = goertzel(buf, REFERENCE_HZ, ctx.sampleRate);
    acc.reference.sum += r; acc.reference.peak = Math.max(acc.reference.peak, r); acc.reference.n++;
    let sq = 0;
    for (let i = 0; i < buf.length; i++) sq += buf[i] * buf[i];
    const rms = Math.sqrt(sq / buf.length);
    acc.rms.sum += rms; acc.rms.peak = Math.max(acc.rms.peak, rms); acc.rms.n++;
    await new Promise((r2) => setTimeout(r2, 30));
  }
  const out = {};
  for (const [name, a] of Object.entries(acc)) {
    out[name] = {
      mean: Number((a.sum / Math.max(1, a.n)).toPrecision(3)),
      peak: Number(a.peak.toPrecision(3)),
    };
  }
  return out;
}

async function tone(hz, seconds, gain) {
  const osc = ctx.createOscillator();
  const g = ctx.createGain();
  osc.frequency.value = hz;
  g.gain.value = gain;
  osc.connect(g).connect(ctx.destination);
  osc.start();
  await new Promise((r) => setTimeout(r, seconds * 1000));
  osc.stop();
  osc.disconnect();
  g.disconnect();
}

// ---------------------------------------------------------------------
// The real endpoint
// ---------------------------------------------------------------------
let endpoint = null;

async function runModem(step) {
  await teardownModem();
  const role = step.role === 'answer' ? Role.ANSWER : Role.ORIGINATE;
  const timeline = [];
  const mark = (what, extra) => {
    const entry = { t: Number(performance.now().toFixed(0)), what, ...extra };
    timeline.push(entry);
    log(`${what} ${extra ? JSON.stringify(extra) : ''}`);
  };

  endpoint = new ModemEndpoint();
  endpoint.addEventListener('status', (e) => {
    const { state, stage, hasTurn, carrier } = e.detail;
    mark('status', { state, stage, hasTurn, carrier });
  });
  endpoint.addEventListener('data', (e) => {
    mark('data', { text: new TextDecoder().decode(e.detail).slice(0, 80) });
  });
  endpoint.addEventListener('error', (e) => mark('error', { detail: String(e.detail) }));

  try {
    await endpoint.init({ role, duplex: Duplex.HALF_PING_PONG });
    const diag = await endpoint.openMicrophone();
    mark('microphone', { summary: summariseMicDiagnostics(diag), applied: diag.applied });
    if (role === Role.ORIGINATE) {
      if (step.dial !== false) {
        mark('dialling');
        endpoint.dial('01234');
      }
    } else {
      endpoint.answer();
      mark('answering');
    }
  } catch (err) {
    mark('failed', { error: String(err && err.message ? err.message : err) });
  }

  // Let it run, then send the whole timeline in one report rather than
  // one request per event - a phone on a flaky connection should not be
  // making a hundred POSTs during a measurement.
  const seconds = step.seconds ?? 45;
  await new Promise((r) => setTimeout(r, seconds * 1000));
  await report('modem', { role: step.role || 'originate', seconds, timeline });
  await teardownModem();
}

async function teardownModem() {
  if (!endpoint) return;
  try { await endpoint.stop(); } catch { /* already gone */ }
  endpoint = null;
}

// ---------------------------------------------------------------------
// The poll loop
// ---------------------------------------------------------------------
let running = false;
let lastSignature = null;
let busy = false;

async function executeStep(step, revision) {
  const op = step.op || 'idle';
  setState(`${op} (revision ${revision})`);
  log(`step: ${JSON.stringify(step)}`);
  if (op === 'idle') return;
  if (op === 'reload') { location.reload(); return; }

  await ensureAudio();
  if (op === 'measure') {
    const result = await measure(step.seconds ?? 4);
    await report('measure', { label: step.label ?? null, seconds: step.seconds ?? 4, result });
    log('measured');
    return;
  }
  if (op === 'tone') {
    await tone(step.hz ?? 1270, step.seconds ?? 4, step.gain ?? 0.5);
    await report('tone', { hz: step.hz ?? 1270, seconds: step.seconds ?? 4, gain: step.gain ?? 0.5 });
    log('tone done');
    return;
  }
  if (op === 'toneAndMeasure') {
    // Play and listen at once - this is how a device's own loudspeaker
    // leak into its own microphone gets measured, which is the number
    // the whole acoustic problem turns on.
    const playing = tone(step.hz ?? 1270, step.seconds ?? 4, step.gain ?? 0.5);
    const result = await measure((step.seconds ?? 4) - 0.3);
    await playing;
    await report('toneAndMeasure', { hz: step.hz ?? 1270, gain: step.gain ?? 0.5, result });
    log('tone+measure done');
    return;
  }
  if (op === 'modem') {
    await runModem(step);
    return;
  }
  log(`unknown op: ${op}`);
}

async function poll() {
  if (!running) return;
  if (!busy) {
    try {
      const res = await fetch(`/api/lab/plan?device=${encodeURIComponent(device)}`, { cache: 'no-store' });
      const plan = await res.json();
      // One signature per (revision, step) so a step runs once when it
      // is set, not once per poll - and re-running the same step is a
      // matter of bumping the revision.
      const signature = `${plan.revision}:${JSON.stringify(plan.step)}`;
      if (signature !== lastSignature) {
        lastSignature = signature;
        busy = true;
        try {
          await executeStep(plan.step || { op: 'idle' }, plan.revision);
        } finally {
          busy = false;
        }
      } else {
        setState(`waiting (revision ${plan.revision})`);
      }
    } catch (err) {
      setState(`poll failed: ${err}`);
    }
  }
  setTimeout(poll, POLL_MS);
}

els.join.addEventListener('click', async () => {
  try {
    // Inside the gesture: iOS will not start an AudioContext or grant a
    // microphone outside one.
    await ensureAudio();
    running = true;
    setState('joined, waiting for instructions');
    log('joined');
    poll();
  } catch (err) {
    setState(`could not start: ${err}`);
    log(`join failed: ${err}`);
  }
});

els.stop.addEventListener('click', async () => {
  running = false;
  await teardownModem();
  setState('stopped');
  log('stopped');
});

setState('Not connected. Press Join.');
