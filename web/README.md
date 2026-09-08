# The browser endpoint

The same `modem-core` the desktop binary runs, inside a Web Audio
AudioWorklet - not a demo of the idea, a real Bell 103 endpoint that can
hold a call with `modem --single --acoustic` on someone's desk.

This directory does not yet hold the product page - that is Task 3. What
is here:

- `session.js` - a thin wrapper around modem-wasm's raw C-ABI worklet
  exports (`session_new`, `process_out`/`process_in`, `dial`/`answer`,
  `send`/`receive`, ...). Shared, unchanged, between the main thread and
  the worklet - it touches nothing AudioWorkletGlobalScope lacks.
- `worklet.js` - the `AudioWorkletProcessor` that drives a `Session` on
  the audio rendering thread.
- `modem.js` - the main-thread driver: fetches the WASM bytes, brings up
  the `AudioContext`/`AudioWorkletNode`, opens the microphone raw, and
  exposes `dial`/`answer`/`hangup`/`send`/`stop` plus `status`/`data`/
  `diagnostic`/`error` events.
- `harness.html` - proves the endpoint two ways: a pure-WASM loopback
  (dial, answer, exchange a real message, assert the bytes survive byte
  exact - no audio device involved) and the spike's own gate, a spectral
  peak within three bins of 1270 Hz measured through a real
  `AnalyserNode`, now against the real Session's idle mark rather than a
  placeholder tone.

## Building the WASM module

```bash
cargo build --release --target wasm32-unknown-unknown -p modem-wasm --no-default-features
cp target/wasm32-unknown-unknown/release/modem_wasm.wasm web/modem.wasm
```

`web/modem.wasm` is a build artifact and is gitignored, the same
convention `spike/pkg*` already uses.

## Running the harness

AudioWorklet needs a secure context - see `spike/README.md` finding 2.
`spike/serve.py` issues a genuinely trusted certificate via
`tailscale cert` and serves whatever directory it is run from:

```bash
sudo tailscale cert "$(tailscale status --json | python3 -c 'import sys,json;print(json.load(sys.stdin)["Self"]["DNSName"].rstrip("."))')"
cd web && python3 ../spike/serve.py
```

Then open `https://<your-machine>.<your-tailnet>.ts.net:8444/harness.html`
and click the button. Both results are also written to
`window.harnessResult` for automation to poll.

## Microphone diagnostics

`ModemEndpoint.openMicrophone()` asks for `echoCancellation`,
`noiseSuppression` and `autoGainControl` all off as a **required**
constraint (`{exact: false}`), not merely a hint - a plain `false` is a
request some hardware and drivers ignore outright. If the device cannot
guarantee that, it retries as a plain request so the demo still runs in
degraded form, then reads back what was actually applied via
`MediaStreamTrack.getSettings()` and reports the difference through the
`diagnostic` event and `summariseMicDiagnostics()`.

**Known-bad devices:** none confirmed yet - this needs a real device
that keeps its own echo cancellation regardless of the constraint before
an entry can be added here honestly. Add one the moment a real device is
found doing this, with the browser, OS and device name.
