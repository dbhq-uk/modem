# Security

## Reporting

Report vulnerabilities through [GitHub's private advisory form](../../security/advisories/new).
Please do not open a public issue for anything exploitable.

## What modem does with your microphone

The acoustic modes need the microphone, and that is the whole of modem's access to anything
sensitive. What it does with it:

- **Audio never leaves your device.** It is demodulated in the page, by WebAssembly, on your own
  machine. There is no upload, no transcription service, no server-side processing. The site's
  Content-Security-Policy allows outbound connections to Google Analytics and nowhere else, and
  analytics receives page views, never audio.
- **The stream is released when you leave.** Pressing Back or Stop stops every track. This was not
  always true: until 17 September 2026 a stream that arrived after you had left was left running,
  and the window was however long you took to answer the permission prompt. It is fixed and there
  is a test that fails if it regresses (`the microphone is released even if permission arrives
  after Back`).
- **Processing is asked to be off, and reported when it is not.** modem requests raw audio with
  echo cancellation, noise suppression and automatic gain control disabled, then reads back what
  the platform actually applied and says so on screen. That is a fidelity measure, not a privacy
  one, but it means the page tells you what your browser is really doing.
- **The one-device demo never opens the microphone at all.** It cross-wires two endpoints in
  software. Only `/originate` and `/receive` ask.

The native binary opens the default input device through `cpal` when run with `--acoustic`, and
does nothing with the samples except demodulate them.

## What the site stores

Nothing, unless you accept the consent dialog, in which case Google Analytics sets its own
cookies. Declining is a real decline: no analytics script is initialised. The modem behaves
identically either way.

## The acoustic lab

`/lab` and its `/lab/api/*` endpoints are an operator tool for calibrating two real devices against
each other. Writes require a key. The key is a bearer token rather than identity - anyone holding
it can write - and what it protects is a free-tier Cloudflare KV quota, not anything confidential.

- No stored report is ever served back by the worker. The operator reads them from KV directly.
- Every key written expires after seven days.
- Requests are capped at 64 KB.
- `/lab/api/plan` is readable without a key. It returns the instruction the devices are already
  carrying out, which is not a secret.

If the key is missing from KV the write endpoints return 503 rather than accepting anything. That
is deliberate: an unconfigured lab is closed, not open.

## Supply chain

Every GitHub Action is pinned to a commit SHA, and Dependabot keeps those pins moving - a pin with
no updater is a pin that rots. Release binaries are built from the commit the tag points at, and
the release notes record that SHA.

The page loads no third-party JavaScript except Google Analytics, gated behind consent.
