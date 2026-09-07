// Runs on the audio rendering thread. No DOM, no fetch, no window.
//
// The module bytes arrive over the message port because fetch and
// WebAssembly.instantiateStreaming are both unavailable in
// AudioWorkletGlobalScope. See spike/README.md.
class ModemProcessor extends AudioWorkletProcessor {
  constructor() {
    super();
    this.ready = false;
    this.phase = 0;
    this.port.onmessage = async (e) => {
      if (e.data.type !== 'wasm') return;
      try {
        const module = await WebAssembly.compile(e.data.bytes);

        // The import object is empty on purpose. A module built with the
        // wasm-bindgen surface enabled declares imports that resolve
        // against generated JS glue, and WASM resolves every declared
        // import at instantiation time whether or not anything calls it,
        // so such a build fails here immediately.
        const instance = await WebAssembly.instantiate(module, {});
        this.exports = instance.exports;

        // Allocate once. Allocating inside process() would be exactly the
        // real-time sin this architecture exists to avoid.
        this.len = 128;
        this.ptr = this.exports.alloc_f32(this.len);
        this.ready = true;

        this.port.postMessage({
          type: 'ready',
          exports: Object.keys(instance.exports),
        });
      } catch (err) {
        this.port.postMessage({ type: 'error', message: String(err) });
      }
    };
  }

  process(inputs, outputs) {
    const out = outputs[0][0];
    if (!this.ready) {
      out.fill(0);
      return true;
    }
    if (out.length !== this.len) {
      // The render quantum is 128 frames in every current implementation,
      // but do not assume it.
      this.exports.dealloc_f32(this.ptr, this.len);
      this.len = out.length;
      this.ptr = this.exports.alloc_f32(this.len);
    }
    this.phase = this.exports.fill_tone_raw(
      this.ptr, this.len, 1270, sampleRate, this.phase);

    // The view is rebuilt each call because growing linear memory detaches
    // any previously created view.
    out.set(new Float32Array(this.exports.memory.buffer, this.ptr, this.len));
    return true;
  }
}

registerProcessor('modem-processor', ModemProcessor);
