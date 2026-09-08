# The browser endpoint

The same `modem-core` the desktop binary runs, inside a Web Audio
AudioWorklet - not a demo of the idea, a real Bell 103 endpoint that can
hold a call with `modem --single --acoustic` on someone's desk.

This directory also holds `modem.dbhq.uk`'s product page. What is here:

- `index.html` / `style.css` - the product page: the browser demo, the
  "Two ways to try this" split-screen/single-pane section, the
  phase-by-phase explainer and downloads.
- `_gen/frames.py` - regenerates the two rendered terminal frames inside
  the "Two ways to try this" section - see that script's own doc and
  "The generated frames" below.
- `session.js` - a thin wrapper around modem-wasm's raw C-ABI worklet
  exports (`session_new`, `process_out`/`process_in`, `dial`/`answer`,
  `send`/`receive`, ...). Shared, unchanged, between the main thread and
  both worklets - it touches nothing AudioWorkletGlobalScope lacks.
- `worklet.js` - the `AudioWorkletProcessor` that drives one `Session`,
  fed by a real microphone, on the audio rendering thread - the "Two
  devices" endpoint.
- `modem.js` - the main-thread driver for `worklet.js`: fetches the WASM
  bytes, brings up the `AudioContext`/`AudioWorkletNode`, opens the
  microphone raw, and exposes `dial`/`answer`/`hangup`/`send`/`stop`
  plus `status`/`data`/`diagnostic`/`error` events.
- `wired-worklet.js` - the `AudioWorkletProcessor` for the "One device"
  live demo: two `Session`s cross-wired directly in software, each
  one's `process_out` fed into the other's `process_in` and both mixed
  to the speakers - the browser mirror of
  `modem-audio/src/transport.rs`'s `WiredTransport::step`. No
  microphone input at all.
- `wired.js` - the main-thread driver for `wired-worklet.js`: same shape
  as `modem.js` minus the microphone, exposing `dial`/`answer`/`hangup`/
  `send(side, text)`/`stop` plus `status`/`data`/`error` events, where
  `status`/`data` carry both ends' state under `{a, b}`/`{side, bytes}`.
- `harness.html` - proves the endpoint two ways: a pure-WASM loopback
  (dial, answer, exchange a real message, assert the bytes survive byte
  exact - no audio device involved) and the spike's own gate, a spectral
  peak within three bins of 1270 Hz measured through a real
  `AnalyserNode`, now against the real Session's idle mark rather than a
  placeholder tone.

## The generated frames

The two terminal frames inside `index.html`'s "Two ways to try this"
section, between `<!-- BEGIN generated: ... -->` / `<!-- END generated:
... -->` markers, are **generated** - the same rule `brand/_gen/`
follows for the icon and the design tokens. Never hand-edit the markup
inside those markers; edit `web/_gen/frames.py` and/or
`modem-tui/examples/mockup.rs` (the actual source of the rendered
`<pre>` markup - see that example's own module doc) and regenerate:

```bash
python3 web/_gen/frames.py
```

Needs `cargo` on PATH. CI's `generated_assets` job runs this alongside
every generator under `brand/_gen/` and fails on any diff, so a stale
region cannot land unnoticed.

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
