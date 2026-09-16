# The acoustic harness

Two real devices in a room, driven from one place, reporting what they actually hear.

## Why it exists

Two phones on a desk are not a wire. The two-device acoustic mode kept failing in rooms while every test in the workspace passed, and three rounds of fixing it were reasoning against a model rather than a measurement.

The model's central figure was how much louder a device's own loudspeaker is in its own microphone than the far device is. That number was inverse-square arithmetic - about 7x at 2 cm from your own speaker and 15 cm from theirs - and it had never been measured on real hardware. Reasoning from it produced three confident wrong answers:

- **Round one** blamed browser noise suppression, on the entirely plausible grounds that noise suppressors remove sustained tones and this modem is made of them. Modelled and measured, that is false at desk levels.
- **Round two** attenuated the idle tone, which is the right lever, and broke turn handover at every level tried - because the level step landed inside the tail of the `Turn` packet. That needed a second fix (`TAIL_IDLE_GAP_BITS`) before the first one was safe.
- **Round three** set the idle level three times over, from three measurements. All three were beside the point: lowering this end's tone lowers the far end's by exactly as much, so the ratio between them never moved.

The actual fault was one line in `carrier.rs`, and it is described under [The bug this all led to](#the-bug-this-all-led-to). Every round of it was found by building the measurement rather than by thinking harder, which is what the harness makes permanent.

## The simulated half

[`modem-core/tests/acoustic.rs`](../modem-core/tests/acoustic.rs) composes the whole chain a desk-to-desk call goes through - band limiting, a desk reflection, room noise, a device's own loudspeaker leaking into its own microphone, and optionally the browser's noise suppression and automatic gain control - and reports a byte error rate for it.

Its findings, as tests rather than prose:

| Test | What it pins |
|---|---|
| `an_unimpaired_link_is_perfect` | The control. Without it every other number is unreadable - see below. |
| `both_bands_clear_the_self_jam_measured_in_the_field` | Both bands survive 40x their own loudspeaker, against the 22x measured on real hardware. |
| `browser_audio_processing_is_not_what_breaks_this` | Noise suppression and AGC both leave the link at zero errors. |
| `the_room_alone_is_survivable_in_both_bands` | The rest of the channel is not the problem either. |
| `carrier_survives_a_quiet_idle_tone` | What bounds `IDLE_MARK_AMPLITUDE` from below. |

**The asymmetry was real and is now gone.** Each end holds continuous idle mark while it does not have the turn, so the end that is *listening* spends the whole burst jamming itself with its own tone. The interferer is each end's own mark tone, the top of its own band, and the two bands are not equally far from it:

| Receiving end | Listening on | Own mark | Gap |
|---|---|---|---|
| Originate | 2025 / 2225 Hz | 1270 Hz | 755 Hz |
| Answer | 1070 / 1270 Hz | 2225 Hz | 955 Hz |

200 Hz of extra separation is a property of Bell 103's band plan used acoustically, not anything this crate chose, and for a long time it decided which direction failed: the answer band tolerated 1.5x its own loudspeaker where the originate band managed 4x.

Both now clear 40x, the top of the sweep, and they clear it equally. Subtracting a receiver's own band from its carrier decision removed the mechanism rather than the symptom, so the band plan no longer decides anything.

**A harness bug worth remembering.** The first run reported a byte error rate of 1.0 for every condition including the unimpaired one, which reads as "the acoustic path destroys the link". It was the harness: `Rx::new` tunes to `tones(cfg.role.listen())`, the *far* end's band, so pairing the same role on both sides listens to entirely the wrong tones. `an_unimpaired_link_is_perfect` exists because of that and runs first.

## The real half: two pages, two jobs

**`/calibrate`** is a standalone instrument. Press a button, it measures, it prints JSON, you copy it out. No account, no storage, no network beyond the page itself. Reach for it when there is nothing to co-ordinate and you only want to know what one device hears.

**`/lab`** is the same measurements with a wire back. Both devices register, poll for an instruction, carry it out, and post what they measured. It exists because the interesting readings need *both* devices doing a specific thing at the same moment - one holding a tone while the other listens - and co-ordinating that by shouting across a room produces numbers you cannot trust.

Neither page is in the nav or the sitemap, and both are noindexed. They are instruments, not pages.

## Running the lab

Open `/lab` on each device with a name and the key, press Join, allow the microphone, and leave the screens awake:

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
web/_routes.json      restricts the worker to /lab/api/*
scripts/lab.py        the operator side: set the plan, read the reports
infra/main.tf         the KV namespace and its Pages binding
```

Three endpoints, all under `/lab/api/` - namespaced under the page they
serve, so everything the lab owns sits beneath one prefix:

| Endpoint | Method | Purpose |
|---|---|---|
| `/lab/api/hello` | POST | Device registers; records user agent, sample rate and what the microphone actually granted. |
| `/lab/api/plan` | GET | What this device should be doing now. |
| `/lab/api/report` | POST | Measurements and timelines. |

The operator writes the plan straight into KV over Cloudflare's REST API, which is why changing what the devices do takes effect on their next poll instead of on the next deploy. The worker only ever reads the plan; it never writes one.

### `_routes.json` is not optional

It restricts the worker to `/lab/api/*`, so every other path on the site is served by the platform as a static asset and never enters the worker at all. A mistake in the collector can therefore only break the lab.

A `_worker.js` at the root *without* that file puts Pages into advanced mode, where the worker handles every single request - and one bug in it takes the whole site down. Both files, or neither. `scripts/build-dist.sh` copies them together and says so in a comment.

Verify changes to the worker with `npx wrangler pages dev dist` before deploying. It runs the same runtime locally, and the check is that the three endpoints answer *and* that `/` and `/lab` are still served statically.

### Infrastructure

The KV namespace and its Pages binding are declared in `infra/main.tf`, like everything else on this account. `terraform plan` should be a clean no-op.

Two things about that resource are worth knowing before touching it:

- The Pages project's `ignore_changes` no longer lists `deployment_configs`. It did while there was nothing in it, and that was wrong the moment a binding was needed, because a binding is not a deployment. `build_config` and `source` are still ignored, because wrangler owns those.
- `usage_model` is pinned to `standard` explicitly. The provider's own default is the legacy `bundled`, so leaving it out makes every future plan propose a silent downgrade of the account's usage model - a billing change, offered as drift.

## What stops just anybody writing

Writes take a key, passed in the URL as `?key=...` and compared against a value held in KV that appears nowhere in this repository. On the operator's machine it lives in `~/.dbhq/modem-lab-key`, mode 600. Reads are open: the plan endpoint says what the devices have been told to do, which is not worth guarding.

The comparison only runs when a key has been set, so an empty namespace does not lock the operator out of their own instrument. To rotate it, write a new `labkey` into KV and reopen the pages with the new value.

**Be clear about what this is.** It is a bearer token in a URL, held in two address bars and in `localStorage`. Anything a browser can hold, a browser can leak, and anyone who gets the key can write. It is not identity and it does not pretend to be. Cloudflare Access was tried here and taken out again: identity, a login screen and a code in an inbox are the wrong shape for two devices being picked up and put down in a room.

What it does stop is the actual risk, which is a stranger finding a public write endpoint in a public repository and spending a free-tier quota on it.

The bounds that do not depend on the key at all:

- 64 KB a request.
- A seven-day TTL on every key written, so nothing accumulates.
- No endpoint anywhere in the worker that reads stored data back out. The operator reads KV directly over the REST API, which does not go through the site.

So the worst case is somebody who has the key writing rubbish into a namespace nobody serves. It is not a route to reading anyone's measurements.

## The bug this all led to

`ToneDominance` asks whether the far end's mark tone is `DOMINANCE_RATIO` times louder than the wideband energy on the line. Over a wire that is exactly the right question. Over a room, the wideband figure contained the receiver's own loudspeaker - six to nine times louder than the far device, measured - so the test was asking whether the far end was forty times louder than this end's own voice.

It never is, at any volume, because that ratio is set by where two devices are sitting. Every level chosen before this was adjusting both sides of a comparison that could not come out right.

A device's own tones are *known*, not noise, so they are now measured with two more Goertzels and subtracted before the comparison. Parseval gives the conversion between a single-bin power and a contribution to a sum of squares.

**The inverse bug is worth knowing about**, because the first attempt had it. Clamping that subtraction at zero means a block containing nothing but the receiver's own tone leaves a zero denominator, and that tone's faint leakage into the wanted probe reads as an infinitely dominant far end. A receiver that hears itself and calls it a carrier is worse than one that hears nothing. So the denominator is floored at a fraction of the own-band energy: measured, a pure own-band tone puts 1.9% of its power into the wanted probe at worst, and against a ratio of 40 the floor only has to exceed 0.019/40.

Two consequences worth recording. `IDLE_MARK_AMPLITUDE` is now free to be chosen for human comfort rather than for detection, because level and self-jam are finally independent - carrier holds to 0.01 whether the receiver's own tone is absent, 6x louder or 9x louder. And the adaptive backoff added in the round before this was removed: it turned a deaf end's own tone down, which addressed a symptom of this bug and, once the bug was fixed, could only make an end harder for the far side to hear.

## Related

- [`docs/ber-calibration.md`](ber-calibration.md) - the release gate the simulated half is measured against.
- `modem-core/src/impair.rs` - the impairment models, including `noise_suppression` and `agc`.
- `modem-core/src/session.rs` - `IDLE_MARK_AMPLITUDE`, `GRANT_IDLE_GAP_BITS` and `TAIL_IDLE_GAP_BITS`, the three constants this work produced.
