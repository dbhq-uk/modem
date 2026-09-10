// Acoustic calibration instrument for modem.dbhq.uk/calibrate.
//
// Deliberately standalone: no worklet, no WASM, no Session. Every one of
// those is a thing that can fail or colour the measurement, and the
// question here is what the microphone and the room are actually doing.
// An AudioContext, a raw microphone, an AnalyserNode and a Goertzel are
// the whole instrument.
//
// It exists because the two-device acoustic mode kept failing in the
// field while every model said it should work, and the model's central
// number - how much louder a device's own loudspeaker is in its own
// microphone than the far device is - was an estimate from inverse
// square, never a measurement. This measures it.

const TONES = {
  'originate space (1070)': 1070,
  'originate mark (1270)': 1270,
  'answer space (2025)': 2025,
  'answer mark (2225)': 2225,
};
/// Off-band reference, so a level can be read against this room's own
/// floor rather than against an absolute that means nothing across
/// devices.
const REFERENCE_HZ = 3000;

const results = {
  version: 2,
  when: null,
  device: null,
  microphone: null,
  steps: {},
};

const out = document.getElementById('out');
const meter = document.getElementById('meter');
const buttons = [...document.querySelectorAll('.dial-button')];

function render() {
  out.textContent = JSON.stringify(results, null, 2);
}

function say(text) {
  meter.textContent = text;
}

function busy(on) {
  for (const b of buttons) b.disabled = on;
}

/** Goertzel magnitude of `freq` in `samples`, normalised by length. */
function goertzel(samples, freq, rate) {
  const k = (2 * Math.PI * freq) / rate;
  const coeff = 2 * Math.cos(k);
  let s0 = 0;
  let s1 = 0;
  let s2 = 0;
  for (let i = 0; i < samples.length; i++) {
    s0 = samples[i] + coeff * s1 - s2;
    s2 = s1;
    s1 = s0;
  }
  const real = s1 - s2 * Math.cos(k);
  const imag = s2 * Math.sin(k);
  return (2 * Math.hypot(real, imag)) / samples.length;
}

function rmsOf(samples) {
  let acc = 0;
  for (let i = 0; i < samples.length; i++) acc += samples[i] * samples[i];
  return Math.sqrt(acc / samples.length);
}

let ctx = null;
let micNode = null;
let analyser = null;

/**
 * Opens the microphone with the same constraints the real endpoint asks
 * for, and records what the device actually granted. A device that
 * silently keeps its noise suppression on is a fact worth having, not a
 * detail to route around.
 */
async function ensureMic() {
  if (analyser) return;
  const AudioCtor = window.AudioContext || window.webkitAudioContext;
  // play-and-record, not playback: on iOS `playback` is output-only and
  // getUserMedia is refused outright while it is set.
  try {
    if ('audioSession' in navigator) navigator.audioSession.type = 'play-and-record';
  } catch (err) {
    results.device = { ...(results.device || {}), audioSessionError: String(err) };
  }
  ctx = new AudioCtor();
  await ctx.resume();

  let stream;
  const raw = {
    echoCancellation: { exact: false },
    noiseSuppression: { exact: false },
    autoGainControl: { exact: false },
  };
  let exact = true;
  try {
    stream = await navigator.mediaDevices.getUserMedia({ audio: raw });
  } catch (err) {
    exact = false;
    stream = await navigator.mediaDevices.getUserMedia({
      audio: { echoCancellation: false, noiseSuppression: false, autoGainControl: false },
    });
  }
  const track = stream.getAudioTracks()[0];
  const settings = track.getSettings ? track.getSettings() : {};
  results.microphone = {
    requestedExact: exact,
    echoCancellation: settings.echoCancellation ?? null,
    noiseSuppression: settings.noiseSuppression ?? null,
    autoGainControl: settings.autoGainControl ?? null,
    label: track.label || null,
    sampleRate: settings.sampleRate ?? null,
  };
  results.device = {
    ...(results.device || {}),
    userAgent: navigator.userAgent,
    contextSampleRate: ctx.sampleRate,
    audioSession: 'audioSession' in navigator ? navigator.audioSession.type : null,
  };
  results.when = new Date().toISOString();

  micNode = ctx.createMediaStreamSource(stream);
  analyser = ctx.createAnalyser();
  analyser.fftSize = 2048;
  micNode.connect(analyser);
}

/**
 * Measures the four Bell 103 tones and an off-band reference for
 * `seconds`, returning the peak and mean of each.
 *
 * Peak as well as mean because the two answer different questions: mean
 * says what the receiver has to live with continuously, peak says
 * whether the tone was ever heard at all.
 */
async function measure(seconds, label) {
  const buf = new Float32Array(analyser.fftSize);
  const acc = {};
  for (const name of Object.keys(TONES)) acc[name] = { sum: 0, peak: 0, n: 0 };
  acc.reference = { sum: 0, peak: 0, n: 0 };
  acc.rms = { sum: 0, peak: 0, n: 0 };

  const until = performance.now() + seconds * 1000;
  while (performance.now() < until) {
    analyser.getFloatTimeDomainData(buf);
    for (const [name, hz] of Object.entries(TONES)) {
      const v = goertzel(buf, hz, ctx.sampleRate);
      acc[name].sum += v;
      acc[name].peak = Math.max(acc[name].peak, v);
      acc[name].n++;
    }
    const ref = goertzel(buf, REFERENCE_HZ, ctx.sampleRate);
    acc.reference.sum += ref;
    acc.reference.peak = Math.max(acc.reference.peak, ref);
    acc.reference.n++;
    const r = rmsOf(buf);
    acc.rms.sum += r;
    acc.rms.peak = Math.max(acc.rms.peak, r);
    acc.rms.n++;

    const left = Math.max(0, Math.round((until - performance.now()) / 1000));
    say(`${label} - ${left}s`);
    await new Promise((r2) => setTimeout(r2, 40));
  }

  const summary = {};
  for (const [name, a] of Object.entries(acc)) {
    summary[name] = {
      mean: Number((a.sum / Math.max(1, a.n)).toPrecision(3)),
      peak: Number(a.peak.toPrecision(3)),
    };
  }
  return summary;
}

/** Plays `hz` at `gain` for `seconds`. */
async function play(hz, seconds, gain) {
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

/**
 * Plays each of the four tones in turn while measuring all four, so one
 * pass gives both the path's gain at each frequency and how much each
 * tone spills into the others' bins.
 */
async function sweep(label, gain) {
  const per = {};
  for (const [name, hz] of Object.entries(TONES)) {
    const playing = play(hz, 2, gain);
    per[name] = await measure(1.6, `${label}: ${name}`);
    await playing;
  }
  return per;
}

document.getElementById('step-room').addEventListener('click', async () => {
  busy(true);
  try {
    await ensureMic();
    results.steps.room = await measure(4, 'Room, stay quiet');
    say('Room measured.');
  } catch (err) {
    results.steps.room = { error: String(err) };
    say(`Failed: ${err}`);
  }
  busy(false);
  render();
});

document.getElementById('step-self').addEventListener('click', async () => {
  busy(true);
  try {
    await ensureMic();
    results.steps.ownSpeaker = await sweep('Own speaker', 0.5);
    say('Own speaker measured.');
  } catch (err) {
    results.steps.ownSpeaker = { error: String(err) };
    say(`Failed: ${err}`);
  }
  busy(false);
  render();
});

document.getElementById('step-play').addEventListener('click', async () => {
  busy(true);
  try {
    await ensureMic();
    for (const [name, hz] of Object.entries(TONES)) {
      say(`Playing ${name} - press Listen on the other device`);
      await play(hz, 3, 0.5);
    }
    results.steps.playedForFarEnd = true;
    say('Played. Now press Listen here and Play there.');
  } catch (err) {
    say(`Failed: ${err}`);
  }
  busy(false);
  render();
});

document.getElementById('step-listen').addEventListener('click', async () => {
  busy(true);
  try {
    await ensureMic();
    const per = {};
    for (const name of Object.keys(TONES)) {
      per[name] = await measure(3, `Listening for ${name}`);
    }
    results.steps.farEnd = per;
    say('Far end measured.');
  } catch (err) {
    results.steps.farEnd = { error: String(err) };
    say(`Failed: ${err}`);
  }
  busy(false);
  render();
});

document.getElementById('copy').addEventListener('click', async () => {
  const text = JSON.stringify(results, null, 2);
  try {
    await navigator.clipboard.writeText(text);
    say('Copied. Paste it back to Claude.');
  } catch (err) {
    say('Could not copy automatically - select the text below and copy it.');
  }
});

document.getElementById('reset').addEventListener('click', () => {
  results.steps = {};
  render();
  say('Cleared.');
});

render();
