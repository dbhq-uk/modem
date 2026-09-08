#!/usr/bin/env python3
"""Generates brand/wordmark.svg from brand/tokens.json.

Edit this, then regenerate - never hand-edit the SVG. Same rule
`brand/_gen/icon.py` and `brand/_gen/tokens.py` already follow.

    python3 brand/_gen/wordmark.py

## Why a dot-matrix font, not a real one

"`modem`, outlined, no font dependency" - the design spec's own words for
this asset (see brand/README.md and docs/superpowers/specs/2026-09-07-
modem-design.md, "The design system" table). A `<text>` element would
depend on whatever font a viewer's system happens to have; this instead
draws the word as a small hand-authored 5x7 dot-matrix bitmap font,
rendered as plain `<rect>` cells, the same "no font, just shapes" approach
`icon.py` already takes for the mark. It also happens to suit the theme:
a segmented dot-matrix is what an actual 1980s modem's own status display
looked like, which is a more period-correct reference than any installed
monospace face would be.

This is a **standalone brand asset** - a portable logotype for contexts
that cannot use live text (a README header, a social preview image). The
page itself does the opposite on purpose: `web/index.html` renders the
wordmark as real HTML text over the hero image, specifically so it stays
sharp at any size and editable without regenerating anything. Both are
correct; they are for different jobs.

## Colour and glow come from the token file, not fresh literals

Same reasoning `tokens.py` gives for reading `tokens.json` rather than
declaring its own values: this is the second consumer of the brand
palette, and a fresh literal here is exactly the drift the token system
exists to prevent.
"""

import json
from pathlib import Path

HERE = Path(__file__).resolve().parent
BRAND = HERE.parent
TOKENS_JSON = BRAND / "tokens.json"

# 5 columns x 7 rows, top to bottom. 1 = lit cell. Only the four distinct
# glyphs "modem" needs are defined - `m` is reused for the first and last
# letter.
GLYPHS = {
    "m": [
        "00000",
        "00000",
        "11010",
        "10101",
        "10101",
        "10101",
        "10101",
    ],
    "o": [
        "00000",
        "00000",
        "01110",
        "10001",
        "10001",
        "10001",
        "01110",
    ],
    "d": [
        "00001",
        "00001",
        "00001",
        "01111",
        "10001",
        "10001",
        "01110",
    ],
    "e": [
        "00000",
        "00000",
        "01110",
        "10001",
        "11111",
        "10000",
        "01111",
    ],
}

WORD = "modem"
COLS_PER_GLYPH = 5
ROWS = 7
GAP_COLS = 1
CELL = 8  # SVG units per dot
RADIUS = 1.5  # rounding on each lit dot - "chiclet" LED look
PAD = CELL * 2  # margin around the whole word


def load_tokens() -> dict:
    return json.loads(TOKENS_JSON.read_text())


def dots():
    """Yields (x, y) grid coordinates (in cells) for every lit dot across
    the whole word, left to right."""
    col_offset = 0
    for letter in WORD:
        pattern = GLYPHS[letter]
        for row, bits in enumerate(pattern):
            for col, bit in enumerate(bits):
                if bit == "1":
                    yield (col_offset + col, row)
        col_offset += COLS_PER_GLYPH + GAP_COLS


def svg(tokens: dict) -> str:
    ground = tokens["colour"]["ground"]
    amber = tokens["colour"]["phosphor"]["amber"]
    bloom = tokens["crt"]["bloom_radius_px"]

    total_cols = len(WORD) * COLS_PER_GLYPH + (len(WORD) - 1) * GAP_COLS
    width = total_cols * CELL + PAD * 2
    height = ROWS * CELL + PAD * 2

    cells = list(dots())
    lit = "\n    ".join(
        f'<rect x="{PAD + gx * CELL}" y="{PAD + gy * CELL}" '
        f'width="{CELL - 1}" height="{CELL - 1}" rx="{RADIUS}"/>'
        for gx, gy in cells
    )

    return f"""<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {width} {height}"
     width="{width}" height="{height}" role="img" aria-label="modem">
  <title>modem</title>
  <rect width="{width}" height="{height}" fill="{ground}"/>
  <g filter="url(#bloom)" fill="{amber["bright"]}" opacity="0.55">
    {lit}
  </g>
  <g fill="{amber["bright"]}">
    {lit}
  </g>
  <defs>
    <filter id="bloom" x="-50%" y="-50%" width="200%" height="200%">
      <feGaussianBlur stdDeviation="{bloom}"/>
    </filter>
  </defs>
</svg>
"""


def main() -> None:
    tokens = load_tokens()
    out = BRAND / "wordmark.svg"
    out.write_text(svg(tokens))
    print(f"wrote {out}")


if __name__ == "__main__":
    main()
