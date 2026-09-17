# Contributing

## The one rule

**The seam between performed and real is marked, always.**

modem plays a dial-up call. Part of that call is a performance and part of it is a working Bell 103
modem, and the project's entire claim to being interesting rests on never letting a reader confuse
the two:

- The overture is **performed** to real timings - UK dial tone at 350 and 450 Hz, the DTMF digits,
  double-ring ringback, the 2100 Hz ANSam answer tone with its phase reversals every 450 ms. There
  is no telephone network. The digits are decorative.
- The training that follows is an **impression**. There is no channel to negotiate.
- Everything after `CONNECT 300` is **real**: FSK at 300 baud, originate on 1270/1070 Hz, answer on
  2225/2025 Hz, carrying actual bytes.
- The screech everyone remembers is a V.34 handshake at 33.6k. This is not that, and the site says
  so rather than letting the association do the work.

A change that makes the demo more impressive by blurring that line will be rejected even if it
sounds better. A pull request that makes the seam *clearer* is welcome without any other
justification.

The same instinct applies to measurements. A number in a comment, a doc or on the site should say
where it came from and whether it was measured or modelled. Three confident wrong answers went into
the acoustic fix because a central figure had been calculated from the inverse square law and never
once measured; `/debugging` is the write-up.

## Building

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

The web tests need the built tree:

```bash
cargo build --release --target wasm32-unknown-unknown -p modem-wasm --no-default-features
./scripts/build-dist.sh
npx playwright test
```

`scripts/build-dist.sh` assembles exactly what gets uploaded, and the smoke tests run against that
tree rather than against the source - so what is tested is what ships.

## Generated files

Three things are generated and must never be hand-edited:

| Generator | What it owns |
|---|---|
| `web/_gen/pages.py` | the nav, footer and consent block in every `web/*.html`, between BEGIN/END markers |
| `web/_gen/frames.py` | the two rendered terminal frames in `index.html` |
| `web/_gen/icons.py` | the button icons, matched on visible button text |
| `brand/_gen/*.py` | the brand assets |

Edit the generator, run it, commit both. CI runs every script under `brand/_gen/` and `web/_gen/`
and fails on any diff, so a hand-edit inside a marked region is caught - but it is caught after you
have done the work twice.

## Tests

**A test that passes when the feature is deleted is worse than no test**, because it reports
coverage that does not exist. Two in this repo were found that way in a review, and both had
looked fine for weeks.

So: when you add a test for a fix, break the fix and watch the test fail before you trust it. If it
still passes, the test is wrong. Say in the commit message that you checked - several commits here
record exactly that, including one where reverting half the fix was not enough to fail the test and
the other half had to come out too.

Where a number can be checked against the physical world, check it against the physical world. The
self-jam figures come from a Windows laptop and an iPhone on a desk, not from arithmetic.

## Style

- **Plain hyphens only.** No em dashes, no en dashes. CI fails on them.
- **British English.**
- Comments explain *why*, at whatever length that takes. This repo has long comments on purpose:
  most of them record a decision that was got wrong once, and the reason is worth more than the
  brevity.
- Conventional commit prefixes with an optional scope: `feat(web):`, `fix(core):`, `chore(ci):`.

## Deploying

Don't. Merging to `main` runs every check and then deploys, in that order, as one workflow. A
hand-deploy skips the tests and forgets the cache purge.
