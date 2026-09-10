#!/usr/bin/env python3
"""Puts an icon on every `.dial-button` across the site.

    python3 web/_gen/icons.py

Idempotent: a button that already carries one is left alone, so this can
be re-run after editing a page without stacking glyphs.

## Why a generator rather than hand-editing seven files

The same buttons recur across pages - "Try the demo" appears three
times, "Read the phase-by-phase explainer" twice - and a hand-edited copy
drifts the first time one of them changes. Same reasoning as
web/_gen/pages.py for the shared chrome, and CI's `generated_assets` job
already re-runs every script under web/_gen/ and fails on a diff, so this
needed no new wiring there.

## The icons themselves

One line-drawn glyph per button, 24x24, `currentColor`, no fills. They
inherit the button's own colour, which means they inherit every state -
secondary, hover, focus, disabled - without a second set of rules to keep
in step. `stroke-width: 2` throughout so a phosphor screen renders them
as marks rather than as illustrations.

Matched on the button's text, not its id: the text is what a reader sees
and what decides which glyph is right, and several of these buttons have
no id at all.
"""

import re
from pathlib import Path

HERE = Path(__file__).resolve().parent
WEB = HERE.parent

PAGES = (
    "index.html",
    "explained.html",
    "prior-art.html",
    "downloads.html",
    "projects.html",
    "about.html",
    "404.html",
)


def svg(body: str) -> str:
    return (
        '<svg class="dial-button__icon" viewBox="0 0 24 24" fill="none" '
        'stroke="currentColor" stroke-width="2" stroke-linecap="round" '
        'stroke-linejoin="round" aria-hidden="true" focusable="false">'
        f"{body}</svg>"
    )


# Line art, one per label. Order matters: the first label that is a
# substring of the button's text wins, so the more specific ones come
# first.
ICONS = (
    # A screen split into two panes - what the demo actually shows.
    ("Demo", '<rect x="2" y="4" width="20" height="16" rx="1"/><path d="M12 4v16"/>'),
    # A speaker pushing sound out.
    ("Open originating modem", '<path d="M4 9v6h4l5 4V5L8 9H4Z"/><path d="M17 8a5 5 0 0 1 0 8"/><path d="M20 5a9 9 0 0 1 0 14"/>'),
    # A microphone taking sound in.
    ("Open receiving modem", '<rect x="9" y="2" width="6" height="12" rx="3"/><path d="M5 11a7 7 0 0 0 14 0"/><path d="M12 18v4"/>'),
    # A handset going off-hook to place a call.
    ("Dial now", '<path d="M3 5a2 2 0 0 1 2-2h2l2 5-2 1a12 12 0 0 0 6 6l1-2 5 2v2a2 2 0 0 1-2 2A16 16 0 0 1 3 5Z"/>'),
    # Play.
    ("Start", '<path d="M6 4l14 8-14 8V4Z"/>'),
    ("Play", '<path d="M6 4l14 8-14 8V4Z"/>'),
    # A speaker, for restoring sound.
    ("Resume sound", '<path d="M4 9v6h4l5 4V5L8 9H4Z"/><path d="M17 9a4 4 0 0 1 0 6"/>'),
    # Two sheets, for copying.
    ("Copy diagnostics", '<rect x="9" y="9" width="12" height="12" rx="1"/><path d="M5 15H4a1 1 0 0 1-1-1V4a1 1 0 0 1 1-1h10a1 1 0 0 1 1 1v1"/>'),
    ("Copy link", '<rect x="9" y="9" width="12" height="12" rx="1"/><path d="M5 15H4a1 1 0 0 1-1-1V4a1 1 0 0 1 1-1h10a1 1 0 0 1 1 1v1"/>'),
    # An arrow back.
    ("Back to modem", '<path d="M19 12H5"/><path d="M12 19l-7-7 7-7"/>'),
    ("Back", '<path d="M19 12H5"/><path d="M12 19l-7-7 7-7"/>'),
    # An ear, for the pages about listening.
    ("Hear every phase explained", '<path d="M6 8a6 6 0 1 1 12 0c0 3-2 4-3 6s-1 4-3 4a3 3 0 0 1-3-3"/><path d="M9 8a3 3 0 0 1 6 0"/>'),
    ("Read the phase-by-phase explainer", '<path d="M4 5a2 2 0 0 1 2-2h6v18H6a2 2 0 0 1-2-2V5Z"/><path d="M12 3h6a2 2 0 0 1 2 2v14a2 2 0 0 1-2 2h-6"/>'),
    ("Standards, prior art and history", '<path d="M4 5a2 2 0 0 1 2-2h6v18H6a2 2 0 0 1-2-2V5Z"/><path d="M12 3h6a2 2 0 0 1 2 2v14a2 2 0 0 1-2 2h-6"/>'),
    ("Try the demo", '<rect x="2" y="4" width="20" height="16" rx="1"/><path d="M12 4v16"/>'),
    # Downward arrow into a tray.
    ("Releases on GitHub", '<path d="M12 3v12"/><path d="M7 10l5 5 5-5"/><path d="M4 21h16"/>'),
)


def icon_for(text: str) -> str | None:
    for label, body in ICONS:
        if label in text:
            return svg(body)
    return None


# A `.dial-button` <button> or <a> whose content is plain text - i.e. one
# that has not been given an icon already.
BUTTON = re.compile(
    r'<(?P<tag>button|a)(?P<attrs>[^>]*class="[^"]*\bdial-button\b[^"]*"[^>]*)>'
    r"(?P<text>[^<]+)"
    r"</(?P=tag)>"
)


def main() -> None:
    total = 0
    for name in PAGES:
        path = WEB / name
        html = path.read_text()

        def replace(match: re.Match) -> str:
            text = match.group("text").strip()
            mark = icon_for(text)
            if mark is None:
                return match.group(0)
            tag, attrs = match.group("tag"), match.group("attrs")
            return f"<{tag}{attrs}>{mark}<span>{text}</span></{tag}>"

        new, n = BUTTON.subn(replace, html)
        if n:
            path.write_text(new)
        # subn counts every match, including the ones left alone.
        added = new.count('class="dial-button__icon"') - html.count('class="dial-button__icon"')
        total += added
        print(f"{name}: {added} icon(s) added")
    print(f"{total} added in total")


if __name__ == "__main__":
    main()
