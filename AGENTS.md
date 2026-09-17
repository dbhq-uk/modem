# Working in this repo

Read [`CONTRIBUTING.md`](CONTRIBUTING.md) first - the one rule and the style rules there apply to
you too. This file is the things an agent gets wrong in this repo specifically.

## Orient before editing

- `README.md` for what the project is and which parts of the call are real.
- `web/README.md` for the site's structure.
- `docs/acoustic-harness.md` for the two-device lab and the self-jam measurements.
- `/debugging` on the live site, or `web/debugging.html`, for how the acoustic bug was actually
  found. It is the best short account of how this codebase expects problems to be approached.

## The traps, in the order they catch people

**1. Never hand-edit a generated region.** `web/_gen/pages.py` owns the nav, footer and consent
block inside the `<!-- BEGIN generated: ... -->` markers of every page. `web/_gen/frames.py` owns
the terminal frames. `web/_gen/icons.py` owns the button icons. Edit the generator and re-run it.
CI fails on any diff, so the only thing hand-editing buys you is doing the work twice.

**2. A receiver tunes to the far end's band.** `Rx::new` uses `tones(cfg.role.listen())`. Pairing
`Tx(role)` with `Rx(role)` decodes noise and reports total failure for every condition, which reads
as "the acoustic path is hopeless". This has already happened once - see `receive()` in
`modem-core/tests/acoustic.rs` and the control test that caught it.

**3. Break the fix and watch your test fail.** See CONTRIBUTING. This is the single highest-value
habit in this repo and the one most often skipped.

**4. A YAML parser is not a workflow validator.** `yaml.safe_load` accepts duplicate keys and keeps
the last one, so a hand-edited workflow can pass a local check and be rejected by GitHub with no
log. `actionlint` runs in CI; run it locally before pushing workflow changes:

```bash
docker run --rm -v "$PWD":/repo -w /repo rhysd/actionlint:latest
```

**5. Cache-bust when checking the live site.** `curl https://modem.dbhq.uk/` can be served a stale
edge copy. Append `?cb=$RANDOM`.

**6. Don't deploy by hand.** Merging to `main` runs every check and then deploys, as one workflow.

## Conventions

- Stay on the branch the session is already on. Don't create branches or worktrees unprompted.
- Plain hyphens. CI greps for em and en dashes across `.rs`, `.md`, `.js`, `.html`, `.toml`, `.yml`.
- British English.
- `bbs` is lowercase everywhere in prose, per the DBHQ naming rule. The one exception is the nav
  label, which reads `BBS` as the category rather than the project name; `web/_gen/pages.py` says
  so at the entry. If you are "correcting" it, read that comment first.
- Long explanatory comments are the house style, not clutter. Most record a decision that was got
  wrong once. Don't compress them away; do keep them true when the code moves.

## Open work

Issues labelled `review-2026-09-16` came out of a full end-to-end review. Issue #16 is the index
and lists what was checked and what was deliberately not filed.
