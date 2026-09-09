#!/usr/bin/env python3
"""Generates the modem icon and the favicons rendered from it.

Edit this, then regenerate - never hand-edit the SVG or the PNGs. Same
rule the rest of this project's generated assets follow.

## The mark

One phosphor trace on a near-black ground, and it is two things at once:

  - **the letter `m`**, lowercase, which is the whole wordmark, and
  - **a signal**: flat, two pulses, flat.

That second reading is not decoration, it is what a Bell 103 call
actually looks like. The line comes in flat because an idle end holds its
carrier up and sits on mark; it pulses because that is data; it goes flat
again because the carrier does not drop when the talking stops. An end
that fell silent between characters would tear the call down - which this
project shipped as a real bug and fixed on 8 Sep 2026.

So the mark is the name, drawn as the thing the name does.

## Why squared rather than curved

The trace switches between two levels with no ramp, because FSK switches
between two tones. A curved `m` would be a nicer letter and a worse
description. It also matches the character-cell aesthetic the terminal
already uses, where everything is drawn from block glyphs.

## Why the glow is a second stroke, not a filter

A Gaussian blur is the first thing a favicon rasteriser drops. A mark
that keeps its bloom in some contexts and loses it in others is worse
than one that never had it, so the glow is an ordinary wider stroke at
low opacity underneath.
"""

import subprocess
from pathlib import Path

# Straight from the design spec's token table.
GROUND = "#0A0A0A"
GREEN = "#33FF33"

HERE = Path(__file__).resolve().parent
BRAND = HERE.parent

# The SVG scales to anything; 64 is just the coordinate space the
# geometry below is expressed in.
SIZE = 64
CORNER = 13

STROKE = 5.5
GLOW_EXTRA = 4.0
GLOW_OPACITY = 0.18

TOP = 20  # the pulse tops
BASE = 45  # the carrier line
LEFT = 17  # first rising edge
RIGHT = 47  # last falling edge
TAIL = 7  # how far the flat carrier runs before and after


def trace() -> str:
    """The single path: carrier, two pulses, carrier.

    The middle stroke goes down to the carrier and straight back up,
    which is what gives the letter its centre stem - one pulse would be
    an `n`.
    """
    mid = (LEFT + RIGHT) / 2
    return (
        f"M {LEFT - TAIL} {BASE} H {LEFT} V {TOP} H {mid} V {BASE} "
        f"V {TOP} H {RIGHT} V {BASE} H {RIGHT + TAIL}"
    )


def svg() -> str:
    d = trace()
    return f"""<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {SIZE} {SIZE}"
     width="{SIZE}" height="{SIZE}" role="img" aria-label="modem">
  <rect width="{SIZE}" height="{SIZE}" rx="{CORNER}" fill="{GROUND}"/>
  <path d="{d}" fill="none" stroke="{GREEN}" stroke-width="{STROKE + GLOW_EXTRA}"
        stroke-opacity="{GLOW_OPACITY}" stroke-linecap="round" stroke-linejoin="round"/>
  <path d="{d}" fill="none" stroke="{GREEN}" stroke-width="{STROKE}"
        stroke-linecap="round" stroke-linejoin="round"/>
</svg>
"""


def main() -> None:
    BRAND.mkdir(parents=True, exist_ok=True)
    icon = BRAND / "icon.svg"
    icon.write_text(svg())
    print(f"wrote {icon}")

    # 16 and 32 for the browser tab, 180 for iOS, 192 and 512 for the
    # web app manifest (192 is the small/maskable Android/Chrome size;
    # 512 doubles as the big one for social cards too).
    pngs = []
    for px in (16, 32, 180, 192, 512):
        png = BRAND / f"icon-{px}.png"
        subprocess.run(
            ["rsvg-convert", "-w", str(px), "-h", str(px), str(icon), "-o", str(png)],
            check=True,
        )
        pngs.append(png)
        print(f"wrote {png}")

    # A multi-resolution .ico as well: some contexts still ask for one,
    # and browsers pick the size they want out of it.
    ico = BRAND / "favicon.ico"
    subprocess.run(
        ["convert", str(BRAND / "icon-32.png"), str(BRAND / "icon-16.png"), str(ico)],
        check=True,
    )
    print(f"wrote {ico}")


if __name__ == "__main__":
    main()
