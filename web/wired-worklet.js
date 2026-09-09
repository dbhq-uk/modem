// Runs on the audio rendering thread, alongside worklet.js but never in
// the same node as it - this is the "one device" live demo's own
// processor, driving two Sessions cross-wired in software exactly the
// way modem-audio/src/transport.rs's WiredTransport::step wires them:
//
//   a.process_out(&mut scratch_a);
//   b.process_out(&mut scratch_b);
//   a.process_in(&scratch_b);
//   b.process_in(&scratch_a);
//   mix[i] = (scratch_a[i] + scratch_b[i]).clamp(-1.0, 1.0);
//
// This is not a second implementation of that idea - it is the same DSP
// (modem-core's real Session, compiled to the same WASM module) run
// twice and cross-fed in JavaScript instead of Rust, for the same
// reason WiredTransport exists at all: the link never has to cross the
// air for a person to hear the handshake and watch it connect.
//
// `session.js` is imported, not inlined, for the same reason worklet.js
// imports it: it touches nothing AudioWorkletGlobalScope lacks, so the
// same file is shared verbatim by every thread that needs a Session.
import { Role, SessionHandle } from './session.js';

class WiredProcessor extends AudioWorkletProcessor {
  constructor() {
    super();
    // `a` is always Role.Originate, `b` always Role.Answer - the two
    // ends of one call, never reassigned once created. Both come from
    // the same compiled WebAssembly.Module (compiled once in
    // handleMessage), instantiated twice so each Session gets its own
    // linear memory - two Sessions sharing one instance would alias
    // their allocations against each other.
    this.a = null;
    this.b = null;
    // Reused every process() call, resized only if the render
    // quantum's length ever changes - see spike/README.md finding 4 and
    // worklet.js's identical discipline for `inScratch`. Allocating a
    // fresh Float32Array per call is exactly the failure this
    // architecture exists to avoid, and it would happen twice here
    // (once per Session) rather than once.
    this.scratchA = null;
    this.scratchB = null;
    // The last status actually posted for each end, so a message goes
    // out only on a real change - see worklet.js's identical `lastStatus`.
    this.lastStatusA = null;
    this.lastStatusB = null;

    this.port.onmessage = (e) => this.handleMessage(e.data);
  }

  async handleMessage(msg) {
    try {
      switch (msg.type) {
        case 'wasm': {
          // fetch and WebAssembly.instantiateStreaming do not exist here
          // (spike/README.md finding 3) - the bytes arrived over the
          // port as a transferable ArrayBuffer, already fetched on the
          // main thread. The import object is empty on purpose (finding
          // 1): see session.js's instantiateModemModule doc for why
          // that is also the proof this is the right build.
          const module = await WebAssembly.compile(msg.bytes);
          const instanceA = await WebAssembly.instantiate(module, {});
          const instanceB = await WebAssembly.instantiate(module, {});
          this.a = new SessionHandle(instanceA.exports, {
            sampleRate,
            role: Role.ORIGINATE,
            duplex: msg.duplex,
          });
          this.b = new SessionHandle(instanceB.exports, {
            sampleRate,
            role: Role.ANSWER,
            duplex: msg.duplex,
          });
          this.port.postMessage({ type: 'ready' });
          break;
        }
        case 'dial':
          this.requireSessions().a.dial(msg.bytes);
          break;
        case 'answer':
          this.requireSessions().b.answer();
          break;
        case 'hangup':
          this.requireSessions();
          this.a.hangup();
          this.b.hangup();
          break;
        case 'send':
          this.requireSessions();
          (msg.side === 'b' ? this.b : this.a).send(msg.bytes);
          break;
        case 'yieldTurn':
          this.requireSessions();
          (msg.side === 'b' ? this.b : this.a).yieldTurn();
          break;
        default:
          break;
      }
    } catch (err) {
      this.port.postMessage({ type: 'error', message: String(err) });
    }
  }

  requireSessions() {
    if (!this.a || !this.b) {
      throw new Error('wired command received before the WASM module was ready');
    }
    return this;
  }

  process(inputs, outputs) {
    const out = outputs[0] && outputs[0][0];
    if (!out) return true;

    if (!this.a || !this.b) {
      out.fill(0);
      return true;
    }

    if (!this.scratchA || this.scratchA.length !== out.length) {
      this.scratchA = new Float32Array(out.length);
      this.scratchB = new Float32Array(out.length);
    }

    // The cross-wire itself: each end's outgoing block becomes the
    // other's incoming block. Order matters - both process_out calls
    // must happen before either process_in call, or the second Session
    // fed would receive this block's samples while the first receives
    // last block's, quietly halving the round trip (see transport.rs's
    // own doc on `step`).
    this.a.processOut(this.scratchA);
    this.b.processOut(this.scratchB);
    this.a.processIn(this.scratchB);
    this.b.processIn(this.scratchA);

    for (let i = 0; i < out.length; i++) {
      out[i] = Math.max(-1, Math.min(1, this.scratchA[i] + this.scratchB[i]));
    }

    this.reportStatusIfChanged();
    this.relayReceivedData();

    return true;
  }

  reportStatusIfChanged() {
    const a = this.statusOf(this.a);
    const b = this.statusOf(this.b);
    if (!this.statusEqual(a, this.lastStatusA) || !this.statusEqual(b, this.lastStatusB)) {
      this.lastStatusA = a;
      this.lastStatusB = b;
      this.port.postMessage({ type: 'status', a, b });
    }
  }

  statusOf(session) {
    return {
      state: session.state(),
      stage: session.stage(),
      hasTurn: session.hasTurn(),
      carrier: session.carrierDetected(),
      ringNumber: session.ringNumber(),
    };
  }

  statusEqual(a, b) {
    return (
      !!b &&
      a.state === b.state &&
      a.stage === b.stage &&
      a.hasTurn === b.hasTurn &&
      a.carrier === b.carrier &&
      a.ringNumber === b.ringNumber
    );
  }

  relayReceivedData() {
    const fromA = this.a.receive();
    if (fromA.length > 0) {
      this.port.postMessage({ type: 'data', side: 'a', bytes: fromA }, [fromA.buffer]);
    }
    const fromB = this.b.receive();
    if (fromB.length > 0) {
      this.port.postMessage({ type: 'data', side: 'b', bytes: fromB }, [fromB.buffer]);
    }
  }
}

registerProcessor('wired-processor', WiredProcessor);
