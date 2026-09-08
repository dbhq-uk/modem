# brand

The icon, and the design tokens both surfaces (the TUI and the
`modem.dbhq.uk` page) share.

Everything here except `_gen/` and `tokens.json` is **generated**. Edit
the generator, then regenerate - never hand-edit an SVG, a PNG,
`tokens.css` or `tokens.rs`, the same rule the rest of this project's
generated assets follow. `tokens.json` is the one exception: it is the
hand-edited source the token generator reads.

```bash
python3 brand/_gen/icon.py
python3 brand/_gen/tokens.py
```

Needs `rsvg-convert` and ImageMagick's `convert` for the icon; the
tokens generator needs nothing beyond the Python standard library.

| File | What it is for |
|---|---|
| `icon.svg` | the source render, scales to anything |
| `icon-16.png`, `icon-32.png` | the browser tab |
| `icon-180.png` | iOS home screen |
| `icon-512.png` | manifests, social cards, the org avatar |
| `favicon.ico` | multi-resolution, for anything that still asks |
| `tokens.json` | **hand-edited.** colour, type, spacing and the CRT treatment - the single source both outputs below are generated from |
| `tokens.css` | generated - custom properties the page consumes |
| `tokens.rs` | generated - plain constants `modem-tui/src/tokens.rs` includes verbatim; `modem-tui/src/theme.rs` turns the colours into `ratatui::style::Color` |

## The tokens

One source, two outputs, so the terminal and the page cannot drift
without someone noticing - `modem-tui`'s own `tests/tokens_agree.rs`
parses `tokens.json`, `tokens.css` and `tokens.rs` independently and
fails if any of the three disagree.

The tokens are authored to the terminal's limits, not the browser's: the
terminal is character cells and a restricted palette, so colour, the
character-cell spacing unit and the CRT treatment (scanlines, bloom,
phosphor decay) are all defined in those terms first, and the page adds
glow and scanlines on top rather than the terminal trying to approximate
the page. Phosphor decay in particular is set to match `modem-tui`'s own
waterfall trail persistence (see `brand/_gen/tokens.py`'s own doc for the
derivation) - it is the single strongest CRT cue there is, and the whole
point of a shared token is that the TUI and the page cannot disagree on
it by accident.

## The mark

One phosphor trace on a near-black ground, and it is two things at once:
the letter `m`, lowercase, which is the whole wordmark; and a signal -
flat, two pulses, flat.

That second reading is what a Bell 103 call actually looks like. The line
comes in flat because an idle end holds its carrier up and sits on mark,
it pulses because that is data, and it goes flat again because the
carrier does not drop when the talking stops. An end that fell silent
between characters would tear the call down, which this project shipped
as a real bug and fixed on 8 Sep 2026.

So the mark is the name, drawn as the thing the name does.

Squared rather than curved because FSK switches between two tones with no
ramp. A curved `m` would be a nicer letter and a worse description, and
the squared form matches the character-cell aesthetic the terminal
already uses.
