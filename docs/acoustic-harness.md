# The acoustic harness

Two real devices in a room, driven from one place, reporting what they actually hear.

## Why it exists

Two phones on a desk are not a wire. The two-device acoustic mode kept failing in rooms while every test in the workspace passed, and three rounds of fixing it were reasoning against a model rather than a measurement.

The model's central figure was how much louder a device's own loudspeaker is in its own microphone than the far device is. That number was inverse-square arithmetic - about 7x at 2 cm from your own speaker and 15 cm from theirs - and it had never been measured on real hardware. Reasoning from it produced a confident wrong answer twice:

- **Round one** blamed browser noise suppression, on the entirely plausible grounds that noise suppressors remove sustained tones and this modem is made of them. Modelled and measured, that is false at desk levels.
- **Round two** attenuated the idle tone, which is the right lever, and broke turn handover at every level tried - because the level step landed inside the tail of the `Turn` packet. That needed a second fix (`TAIL_IDLE_GAP_BITS`) before the first one was safe.

Both were found by building the measurement rather than by thinking harder. The harness is that principle made permanent.

## The simulated half

[`modem-core/tests/acoustic.rs`](../modem-core/tests/acoustic.rs) composes the whole chain a desk-to-desk call goes through - band limiting, a desk reflection, room noise, a device's own loudspeaker leaking into its own microphone, and optionally the browser's noise suppression and automatic gain control - and reports a byte error rate for it.

Its findings, as tests rather than prose:

| Test | What it pins |
|---|---|
| `an_unimpaired_link_is_perfect` | The control. Without it every other number is unreadable - see below. |
| `answer_band_tolerates_less_of_its_own_speaker_than_originate_does` | The asymmetry, as a threshold. |
| `browser_audio_processing_is_not_what_breaks_this` | Noise suppression and AGC both leave the link at zero errors. |
| `the_room_alone_is_survivable_in_both_bands` | The rest of the channel is not the problem either. |
| `carrier_survives_a_quiet_idle_tone` | What bounds `IDLE_MARK_AMPLITUDE` from below. |

**The asymmetry is the finding.** Each end holds continuous idle mark while it does not have the turn, so the end that is *listening* spends the whole burst jamming itself with its own tone. The interferer is each end's own mark tone, which is the top of its own band, and the two bands are not equally far from it:

| Receiving end | Listening on | Own mark | Gap | Tolerated |
|---|---|---|---|---|
| Originate | 2025 / 2225 Hz | 1270 Hz | 755 Hz | 12x |
| Answer | 1070 / 1270 Hz | 2225 Hz | 955 Hz | 8x |

200 Hz of extra separation is the whole difference, and it is a property of Bell 103's band plan used acoustically rather than anything this crate chose. Before `IDLE_MARK_AMPLITUDE` those figures were 4x and 1.5x, which is why answer-to-originate was always the direction that failed.

**A harness bug worth remembering.** The first run reported a byte error rate of 1.0 for every condition including the unimpaired one, which reads as "the acoustic path destroys the link". It was the harness: `Rx::new` tunes to `tones(cfg.role.listen())`, the *far* end's band, so pairing the same role on both sides listens to entirely the wrong tones. `an_unimpaired_link_is_perfect` exists because of that and runs first.

## The real half: two pages, two jobs

**`/calibrate`** is a standalone instrument. Press a button, it measures, it prints JSON, you copy it out. No account, no storage, no network beyond the page itself. Reach for it when there is nothing to co-ordinate and you only want to know what one device hears.

**`/lab`** is the same measurements with a wire back. Both devices register, poll for an instruction, carry it out, and post what they measured. It exists because the interesting readings need *both* devices doing a specific thing at the same moment - one holding a tone while the other listens - and co-ordinating that by shouting across a room produces numbers you cannot trust.

Neither page is in the nav or the sitemap, and both are noindexed. They are instruments, not pages.

## Running the lab

Open `/lab` on each device with a name and the shared key, press Join, allow the microphone, and leave the screens awake:

```
https://modem.dbhq.uk/lab?device=phone&key=...
https://modem.dbhq.uk/lab?device=laptop&key=...
```

The device name is only a label; anything is fine, and it is what the plan addresses. The key is kept in `localStorage` after the first load so a reload does not silently drop it.

Then drive them from a machine with the Cloudflare credentials loaded:

```bash
source ~/.dbhq/env.sh

scripts/lab.py devices                  # who has checked in, and what their mic actually did
scripts/lab.py plan --file step.json    # set what each device should do next
scripts/lab.py show                     # the plan as it stands
scripts/lab.py reports --last 5         # what they have posted
scripts/lab.py reports --device phone --full
scripts/lab.py idle                     # stand them down
scripts/lab.py clear                    # bin the reports
```

`devices` is worth running first. It reports each device's context sample rate, its `audioSession` type, and - the part that matters - whether the browser honoured the request for a raw microphone or quietly kept echo cancellation, noise suppression or automatic gain control switched on.

## The plan format

A plan names a step per device, with a fallback for anything unnamed:

```json
{
  "note": "self-leak on the phone, laptop listening",
  "devices": {
    "phone":  { "op": "toneAndMeasure", "hz": 2225, "seconds": 4, "gain": 0.5 },
    "laptop": { "op": "measure", "seconds": 4, "label": "hearing the phone" }
  },
  "default": { "op": "idle" }
}
```

`revision` is added and bumped automatically on every set. A device runs a step once per `(revision, step)` pair, so re-issuing an identical step is a bump rather than a special case, and a device that reloads mid-run does not repeat the step it already did.

## What a device can be told to do

| op | Parameters | What it does |
|---|---|---|
| `idle` | | Nothing. |
| `tone` | `hz`, `seconds`, `gain` | Plays a tone. |
| `measure` | `seconds`, `label` | Goertzel at all four Bell 103 tones plus a 3 kHz off-band reference, mean and peak, and broadband RMS. |
| `toneAndMeasure` | `hz`, `seconds`, `gain` | Plays and listens at once. This is how a device's own speaker into its own microphone gets measured directly, rather than estimated. |
| `modem` | `role`, `seconds`, `dial` | Runs the real `ModemEndpoint` and streams its whole timeline back. |
| `reload` | | Reloads the page. |

Everything is reported against the 3 kHz reference as well as in absolute terms, because absolute levels mean nothing across two different microphones.

### Why `modem` is the one that matters

It is the same worklet, the same WASM and the same code path the site uses - not a reimplementation that could differ from the product. Every status change, diagnostic and decoded byte goes into one timeline with millisecond timestamps, posted as a single report at the end rather than one request per event, because a phone on a flaky connection should not be making a hundred POSTs during a measurement.

So a failing call can be read from both ends at once, against a common clock, instead of reconstructed afterwards from what somebody saw on a screen.

### The iteration loop

With `reload` beside it:

1. Change the Rust.
2. `./scripts/build-dist.sh`, deploy.
3. `scripts/lab.py plan` with `reload` for both devices.
4. `scripts/lab.py plan` with `modem` for both devices.
5. `scripts/lab.py reports` and read both timelines.

The plan changes take effect on the next poll, about a second. Only step 2 costs anything.

## How it is wired

```
web/lab.html          the device page
web/lab.js            registers, polls, executes, reports
web/_worker.js        the collector
web/_routes.json      restricts the worker to /api/lab/*
scripts/lab.py        the operator side: set the plan, read the reports
infra/main.tf         the KV namespace and its Pages binding
```

Three endpoints, all under `/api/lab/`:

| Endpoint | Method | Purpose |
|---|---|---|
| `/api/lab/hello` | POST | Device registers; records user agent, sample rate and what the microphone actually granted. |
| `/api/lab/plan` | GET | What this device should be doing now. |
| `/api/lab/report` | POST | Measurements and timelines. |

The operator writes the plan straight into KV over Cloudflare's REST API, which is why changing what the devices do takes effect on their next poll instead of on the next deploy. The worker only ever reads the plan; it never writes one.

### `_routes.json` is not optional

It restricts the worker to `/api/lab/*`, so every other path on the site is served by the platform as a static asset and never enters the worker at all. A mistake in the collector can therefore only break the lab.

A `_worker.js` at the root *without* that file puts Pages into advanced mode, where the worker handles every single request - and one bug in it takes the whole site down. Both files, or neither. `scripts/build-dist.sh` copies them together and says so in a comment.

Verify changes to the worker with `npx wrangler pages dev dist` before deploying. It runs the same runtime locally, and the check is that the three endpoints answer *and* that `/` and `/lab` are still served statically.

### Infrastructure

The KV namespace and its Pages binding are declared in `infra/main.tf`, like everything else on this account. `terraform plan` should be a clean no-op.

Two things about that resource are worth knowing before touching it:

- The Pages project's `ignore_changes` no longer lists `deployment_configs`. It did while there was nothing in it, and that was wrong the moment a binding was needed, because a binding is not a deployment. `build_config` and `source` are still ignored, because wrangler owns those.
- `usage_model` is pinned to `standard` explicitly. The provider's own default is the legacy `bundled`, so leaving it out makes every future plan propose a silent downgrade of the account's usage model - a billing change, offered as drift.

## What stops it being abused

This repository is public, so the existence and shape of the endpoint are public too.

Writes take a shared key, compared against a value held in KV that appears nowhere in this repository. On the operator's machine it lives in `~/.dbhq/modem-lab-key`, mode 600. The comparison only happens when a key has been set, so an empty namespace does not lock the operator out of their own instrument.

**That is a speed bump, not authentication, and the code says so.** The key rides in a URL held in two address bars, and anything a browser can hold, a browser can leak. What it stops is casual abuse, which is the whole of the actual risk here.

The bounds that do not depend on the key:

- 64 KB a request.
- A seven-day TTL on every key written, so nothing accumulates.
- No endpoint anywhere in the worker that reads stored data back out. The operator reads KV directly over the REST API.

The worst case is somebody spending a free-tier write quota. It is not a route to reading anyone's measurements.

## Related

- [`docs/ber-calibration.md`](ber-calibration.md) - the release gate the simulated half is measured against.
- `modem-core/src/impair.rs` - the impairment models, including `noise_suppression` and `agc`.
- `modem-core/src/session.rs` - `IDLE_MARK_AMPLITUDE`, `GRANT_IDLE_GAP_BITS` and `TAIL_IDLE_GAP_BITS`, the three constants this work produced.
