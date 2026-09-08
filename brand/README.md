# brand

The icon, and the tokens both surfaces share.

Everything here except `_gen/` is **generated**. Edit the generator, then
regenerate - never hand-edit an SVG or a PNG, the same rule the rest of
this project's generated assets follow.

```bash
python3 brand/_gen/icon.py
```

Needs `rsvg-convert` and ImageMagick's `convert`.

| File | What it is for |
|---|---|
| `icon.svg` | the source render, scales to anything |
| `icon-16.png`, `icon-32.png` | the browser tab |
| `icon-180.png` | iOS home screen |
| `icon-512.png` | manifests, social cards, the org avatar |
| `favicon.ico` | multi-resolution, for anything that still asks |

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
