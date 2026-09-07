# AudioWorklet spike

**Result: passed, 7 Sep 2026.** Rust-compiled WASM instantiates inside a Web Audio AudioWorklet and produces verified audio.

This spike is the gate the language choice rests on. The spec requires the DSP to run on the audio rendering thread, not the main thread, and Rust was chosen over Go largely because that path was expected to be cleaner. This proves it before any DSP is written.

## What was verified

Chromium via Paseo, `sampleRate` 48000, served over HTTPS.

```
worklet module loaded, sampleRate=48000
wasm fetched, 24432 bytes
WASM INSTANTIATED IN WORKLET - exports: memory, alloc_f32, dealloc_f32, fill_tone_raw
audio graph running
spectral peak at 1271 Hz (-27.8 dB)
TONE VERIFIED - spike passed
```

The last two lines matter. Instantiation alone proves nothing about whether audio reaches the speakers, so the page routes the worklet's output through an `AnalyserNode` and asserts the spectral peak lands within three bins of 1270 Hz - the Bell 103 originate mark tone. A spike that only logged "instantiated" would have passed against a worklet emitting silence.

**Payload:** 24,432 bytes raw, 9,701 gzipped.

## Four findings that shape the real implementation

### 1. The wasm-bindgen surface and the worklet build cannot be the same artifact

This is the big one, and it is not obvious.

Any `#[wasm_bindgen]` item - even one that never touches a string or an externref - makes wasm-bindgen 0.2.128 emit an `__wbindgen_init_externref_table` import. A `&mut [f32]` parameter adds `copy_to_typed_array` on top. Both resolve against the generated `modem_wasm_bg.js` glue, which does not exist inside `AudioWorkletGlobalScope`.

**WASM resolves every declared import at instantiation time, not just the ones a caller uses.** So a build containing both surfaces fails in the worklet immediately, whether or not the worklet ever calls a bindgen-wrapped function.

The fix is a cargo feature. `browser` is on by default and carries the wasm-bindgen surface for ordinary main-thread callers; `--no-default-features` drops it, leaving only `#[no_mangle] extern "C"` exports and a module with **zero imports**, which is what the worklet loads.

Verify with the import-section check rather than trusting the build:

```bash
cargo build --release --target wasm32-unknown-unknown -p modem-wasm --no-default-features
# then confirm the import section is absent or empty
```

### 2. AudioWorklet requires a secure context

Serving the page over plain HTTP from a LAN address gives `ctx.audioWorklet === undefined` and a bare `TypeError: Cannot read properties of undefined (reading 'addModule')`. The API is not merely restricted, it is absent.

For local work, `tailscale cert` issues a genuinely trusted certificate for the machine's `ts.net` name, so there is no click-through warning - which matters, because a browser treats a cert-error origin as insecure and the API stays hidden. `serve.py` in this directory does that.

For production this is already satisfied: `modem.dbhq.uk` is HTTPS, and the `dbhq.uk` zone runs HSTS with `includeSubDomains` anyway.

### 3. fetch does not exist in the worklet

Neither does `WebAssembly.instantiateStreaming`. The bytes are fetched on the main thread and posted in over the message port as a transferable `ArrayBuffer`, then compiled with `WebAssembly.compile` and instantiated with an empty import object.

### 4. Allocate once, outside process()

The first draft called `alloc_f32` and `dealloc_f32` on every `process()` call. That is a heap allocation per 128-sample render quantum on a real-time thread, which is precisely the failure mode this architecture exists to avoid. The buffer is now allocated when the module is instantiated and reused, resized only if the render quantum ever changes.

One related trap: **growing WASM linear memory detaches every existing `Float32Array` view of it.** The view is therefore rebuilt each call rather than cached. Caching it works fine until the first allocation that grows memory, and then fails in a way that is hard to attribute.

## Running it

```bash
sudo tailscale cert <machine>.<tailnet>.ts.net    # once
cargo build --release --target wasm32-unknown-unknown -p modem-wasm --no-default-features
mkdir -p spike/pkg-worklet
cp target/wasm32-unknown-unknown/release/modem_wasm.wasm spike/pkg-worklet/modem.wasm
cd spike && python3 serve.py
```

Then open `https://<machine>.<tailnet>.ts.net:8444/worklet.html` and click Start.

## Status

This is scaffolding, not shipping code. It stays in the repo because the four findings above are expensive to rediscover, and because it is the evidence behind the language decision. Plan 2 builds the real browser endpoint on this path.
