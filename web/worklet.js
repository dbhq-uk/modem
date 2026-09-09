// Runs on the audio rendering thread. No DOM, no fetch, no window - see
// spike/README.md, which this file follows exactly, now driving a real
// modem-core Session instead of the spike's placeholder sine generator.
//
// `session.js` is imported, not inlined, because it is shared verbatim
// with the main thread (modem.js and the test harness) - it touches
// nothing that AudioWorkletGlobalScope lacks, so the same file can be
// loaded on both sides of the port without drift between them.
import { SessionHandle } from './session.js';

class ModemProcessor extends AudioWorkletProcessor {
  constructor() {
    super();
    this.session = null;
    // Reused every call, resized only if the render quantum's length
    // ever changes - see spike/README.md finding 4. A fresh
    // `new Float32Array(len)` every process() call would itself be the
    // allocation this discipline exists to avoid, so this scratch is
    // owned here rather than created inline in process().
    this.inScratch = null;
    // The last status actually posted, so a message goes out only on a
    // real change - process() runs every ~2.7 ms at 48 kHz, and posting
    // on every call would flood the port for no reason.
    this.lastStatus = null;

    this.port.onmessage = (e) => this.handleMessage(e.data);
  }

  async handleMessage(msg) {
    try {
      switch (msg.type) {
        case 'wasm': {
          // fetch and WebAssembly.instantiateStreaming do not exist here
          // (finding 3) - the bytes arrived over the port as a
          // transferable ArrayBuffer, already fetched on the main
          // thread. The import object is empty on purpose (finding 1):
          // see session.js's instantiateModemModule doc for why that is
          // also the proof this is the right build.
          const module = await WebAssembly.compile(msg.bytes);
          const instance = await WebAssembly.instantiate(module, {});
          this.session = new SessionHandle(instance.exports, {
            sampleRate,
            role: msg.role,
            duplex: msg.duplex,
          });
          this.port.postMessage({ type: 'ready', exports: Object.keys(instance.exports) });
          break;
        }
        case 'dial':
          this.requireSession().dial(msg.bytes);
          break;
        case 'answer':
          this.requireSession().answer();
          break;
        case 'hangup':
          this.requireSession().hangup();
          break;
        case 'send':
          this.requireSession().send(msg.bytes);
          break;
        default:
          break;
      }
    } catch (err) {
      this.port.postMessage({ type: 'error', message: String(err) });
    }
  }

  requireSession() {
    if (!this.session) throw new Error('worklet command received before the WASM module was ready');
    return this.session;
  }

  process(inputs, outputs) {
    const out = outputs[0] && outputs[0][0];
    if (!out) return true;

    if (!this.session) {
      out.fill(0);
      return true;
    }

    if (!this.inScratch || this.inScratch.length !== out.length) {
      this.inScratch = new Float32Array(out.length);
    }
    const input = inputs[0] && inputs[0][0];
    if (input) {
      this.inScratch.set(input);
    } else {
      // No microphone connected yet (the demo half of the page, or the
      // permission has not been granted): feed silence rather than
      // stale samples from a previous, differently sized call.
      this.inScratch.fill(0);
    }

    this.session.processIn(this.inScratch);
    this.session.processOut(out);

    this.reportStatusIfChanged();
    this.relayReceivedData();

    return true;
  }

  reportStatusIfChanged() {
    const session = this.session;
    const status = {
      state: session.state(),
      stage: session.stage(),
      hasTurn: session.hasTurn(),
      carrier: session.carrierDetected(),
      ringNumber: session.ringNumber(),
    };
    const last = this.lastStatus;
    if (
      !last ||
      status.state !== last.state ||
      status.stage !== last.stage ||
      status.hasTurn !== last.hasTurn ||
      status.carrier !== last.carrier ||
      status.ringNumber !== last.ringNumber
    ) {
      this.lastStatus = status;
      this.port.postMessage({ type: 'status', ...status });
    }
  }

  relayReceivedData() {
    const bytes = this.session.receive();
    if (bytes.length > 0) {
      this.port.postMessage({ type: 'data', bytes }, [bytes.buffer]);
    }
  }
}

registerProcessor('modem-processor', ModemProcessor);
