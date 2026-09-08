#!/usr/bin/env python3
"""Generates the two rendered-frame regions in web/index.html's
"Two ways to try this" section, straight out of
modem-tui/examples/mockup.rs.

Edit this, then regenerate - never hand-edit the two marked regions in
web/index.html. Same rule brand/_gen/ already follows for the icon and
the design tokens.

    python3 web/_gen/frames.py

Needs `cargo` on PATH.

## Why this shells out to `cargo run` rather than re-implementing anything

mockup.rs already renders real ratatui frames to per-cell coloured HTML,
straight out of App::render_into - see that example's own module doc.
Re-deriving that here in Python would be a second renderer that could
disagree with the first, exactly the drift brand/_gen/tokens.py exists to
prevent for colours; the same argument applies to frames. So this
script's only job is to run the example, pull out the two frames it
marks specifically for this page, and splice them into index.html
unchanged.

## Why these two frames, and how they're found

mockup.rs's own `main` wraps exactly two `<pre>` renders in
`id="web-frame-one-device"` / `id="web-frame-two-device"` divs, purely
for this script to find - see its own comment there. "One device" is
`split_window`'s own render, byte for byte: `App::split` over the wired
demo transport, so `[DEMO MODE]` is genuinely present. "Two devices" is
a fresh single pane wrapped in `FakeAcousticTransport`, which genuinely
reports `is_acoustic() == true`, so `[DEMO MODE]` is genuinely absent -
see that struct's own doc in mockup.rs for why that is a faithful frame
and not a fabricated one.

Matching on those `id`s (rather than, say, counting `<pre>` tags or
matching on heading text) means this script keeps working even if the
prose or the ordering of mockup.rs's comparison page changes - only the
two `id`s are the contract between the two files.
"""

import re
import subprocess
from pathlib import Path

HERE = Path(__file__).resolve().parent
WEB = HERE.parent
ROOT = WEB.parent
INDEX_HTML = WEB / "index.html"

# One marker pair per frame, matched in web/index.html by
# `splice_region` below. `frame_id` is also the `id` mockup.rs's own
# `main` gives each frame's wrapping div - see this module's own doc.
#
# The two "one-device-mobile-*" frames are the narrow-viewport variant of
# "one-device": the same wired call, rendered as two 72-column single
# panes (originate, answer) instead of one 100-column split, because
# App::split refuses to lay out below 80 columns (MIN_SPLIT_COLUMNS) -
# there is no narrower split render to ask the generator for, only a
# different layout. index.html shows one pair or the other by viewport
# width; see web/style.css's own note on that breakpoint.
FRAMES = ("one-device", "two-device", "one-device-mobile-a", "one-device-mobile-b")


def run_mockup() -> str:
    """Runs the mockup example fresh and returns its stdout - the same
    invocation the example's own module doc documents."""
    result = subprocess.run(
        ["cargo", "run", "--quiet", "-p", "modem-tui", "--example", "mockup"],
        cwd=ROOT,
        check=True,
        capture_output=True,
        text=True,
    )
    return result.stdout


def extract_frame(mockup_html: str, frame_id: str) -> str:
    """Pulls the `<pre>...</pre>` markup out of
    `<div id="web-frame-{frame_id}"><pre>...</pre></div>` - the exact,
    already-escaped output of mockup.rs's own `frame_html`, taken
    verbatim so it can never drift from what the real crate draws."""
    pattern = re.compile(
        rf'<div id="web-frame-{re.escape(frame_id)}">(<pre>.*?</pre>)</div>',
        re.DOTALL,
    )
    match = pattern.search(mockup_html)
    if not match:
        raise SystemExit(
            f"mockup did not emit a frame with id=\"web-frame-{frame_id}\" - "
            "did modem-tui/examples/mockup.rs change shape? See "
            "web/_gen/frames.py's own doc for the contract between the two."
        )
    return match.group(1)


def splice_region(html: str, frame_id: str, replacement: str) -> str:
    """Replaces everything between the BEGIN/END marker comments for
    `frame_id` with `replacement`, leaving the markers and everything
    outside them untouched. Fails loudly if the markers are not found,
    rather than silently leaving index.html unchanged."""
    begin = (
        f"<!-- BEGIN generated: {frame_id} frame "
        "(web/_gen/frames.py - do not hand-edit) -->"
    )
    end = f"<!-- END generated: {frame_id} frame -->"
    pattern = re.compile(re.escape(begin) + r".*?" + re.escape(end), re.DOTALL)
    if not pattern.search(html):
        raise SystemExit(
            f"marker pair not found in {INDEX_HTML}: {frame_id!r} - "
            "expected to find:\n"
            f"  {begin}\n  ...\n  {end}"
        )
    block = f"{begin}\n            {replacement}\n            {end}"
    return pattern.sub(lambda _m: block, html, count=1)


def main() -> None:
    mockup_html = run_mockup()
    index_html = INDEX_HTML.read_text()

    for frame_id in FRAMES:
        frame = extract_frame(mockup_html, frame_id)
        index_html = splice_region(index_html, frame_id, frame)

    INDEX_HTML.write_text(index_html)
    print(f"wrote {INDEX_HTML}")


if __name__ == "__main__":
    main()
