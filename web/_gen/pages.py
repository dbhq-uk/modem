#!/usr/bin/env python3
"""Generates the three shared-chrome regions repeated across modem's six
pages: the primary nav, the footer, and the consent/analytics block.

Edit this, then regenerate - never hand-edit the three marked regions in
web/index.html, web/explained.html, web/prior-art.html, web/downloads.html,
web/projects.html, web/about.html or web/404.html. Same rule
web/_gen/frames.py already follows for the two rendered terminal frames.

    python3 web/_gen/pages.py

## Why this exists

Task 3n split modem.dbhq.uk from two pages into six. Six pages sharing a
nav, a footer and a consent dialog is six hand-maintained copies unless
something splices them in from one source - the same drift risk
frames.py already solves for the terminal frames, just for markup this
project authors itself rather than markup pulled from another crate.
There is no build step for HTML in this repo (the whole point of plain
files with no framework), so this follows the established pattern
instead of inventing a new one: BEGIN/END marker comments in each hand-
authored page, and a generator that owns everything between them. CI's
`generated_assets` job already discovers every script under
`web/_gen/*.py` and fails on any diff - this one needed no new wiring
there, only the diff check itself extended to the new page files (see
.github/workflows/ci.yml).

## What is NOT generated

Each page's own `<head>` (title, description, canonical, OG/Twitter,
JSON-LD), hero, and main content stay hand-authored per file - that is
the actual content of each of the six pages, and templating it away
would be the opposite of "six real pages, each with its own title and
h1". Only the chrome every page shares - the six-item nav, the footer's
byline/groups, and the consent dialog plus its two script tags - is
generated.
"""

from pathlib import Path

HERE = Path(__file__).resolve().parent
WEB = HERE.parent

# One nav item per real page - id, label, href. Order is the order the
# brief's own table gives, and the order every page's nav renders in.
NAV_ITEMS = (
    # "Hear it", not "Try" (Dan, 9 Sep 2026). Try names an effort the
    # visitor has to make; this names what they get, and it says the same
    # thing the page's own h1 does - "Hear the dial-up sound, live". The
    # id stays `try` because it is the internal key, not the label, and
    # renaming it would move every aria-current mapping below for
    # nothing.
    ("try", "Hear it", "/"),
    ("explained", "Explained", "/explained"),
    ("prior-art", "Prior art", "/prior-art"),
    ("downloads", "Downloads", "/downloads"),
    # "DBHQ", not "Projects" (Dan, 10 Sep 2026). The page is titled "Also
    # from DBHQ" and lists the practice and its two sibling experiments,
    # so the nav item names the thing rather than describing the shape of
    # the list. The id and the URL stay `projects`: the id is the internal
    # key for aria-current, and changing the URL would break every link
    # already pointing at /projects, including the sitemap and the
    # sibling sites' own footers.
    ("projects", "DBHQ", "/projects"),
    ("about", "About", "/about"),
)

# Which page each file is, for aria-current="page" - and which of the
# three regions each file actually carries. 404.html gets a nav and a
# footer (so it is not a dead end) but no consent dialog: it is
# noindexed and carries no analytics of its own to gate.
PAGES = {
    "index.html": {"nav_id": "try", "regions": ("nav", "footer", "consent")},
    "explained.html": {"nav_id": "explained", "regions": ("nav", "footer", "consent")},
    "prior-art.html": {"nav_id": "prior-art", "regions": ("nav", "footer", "consent")},
    "downloads.html": {"nav_id": "downloads", "regions": ("nav", "footer", "consent")},
    "projects.html": {"nav_id": "projects", "regions": ("nav", "footer", "consent")},
    "about.html": {"nav_id": "about", "regions": ("nav", "footer", "consent")},
    "404.html": {"nav_id": None, "regions": ("nav", "footer")},
}


def render_nav(current_id: str | None) -> str:
    """The six-item sticky primary nav. `current_id` is None on 404.html,
    where none of the six is "the current page" - a 404 is not one of
    them."""
    links = []
    for item_id, label, href in NAV_ITEMS:
        current = ' aria-current="page"' if item_id == current_id else ""
        links.append(f'  <a class="site-nav__link" href="{href}"{current}>{label}</a>')
    body = "\n".join(links)
    return f'<nav class="site-nav" aria-label="Primary">\n{body}\n</nav>'


# The footer: the required "a DBHQ experiment by..." byline, then one
# link to the source with GitHub's own mark beside it. Identical, word
# for word, on all six pages and on 404.html.
#
# It carried two labelled groups until 10 Sep 2026. "Also from DBHQ" went
# first: /projects carries the same three links and is in the primary
# nav, so repeating them in the footer of all seven pages said it twice
# and made the footer the longest thing on the short pages. Crawling is
# unaffected - /projects is in the nav and the sitemap - though internal
# weight to the siblings drops from sitewide to one page, which is the
# deliberate trade.
#
# That left "This project" as a label over a single GitHub link whose URL
# the byline directly above already carried: a heading, a lot of vertical
# space, and one orphaned word (Dan: "foot looks crap still"). Now one
# marked link, said once.
FOOTER = """<footer class="endorsement">
  <p class="endorsement__byline">a <a href="https://dbhq.uk">DBHQ</a> experiment by <a href="https://dbhq.uk">Daniel Grimes</a>. The full workspace - modem-core, modem-audio, modem-tui, modem-wasm and this page - lives on <a href="https://github.com/dbhq-uk/modem" rel="noopener">GitHub</a>, MIT licensed.</p>

  <p class="endorsement__source">
    <a class="endorsement__github" href="https://github.com/dbhq-uk/modem" rel="noopener">
      <svg class="endorsement__github-mark" viewBox="0 0 16 16" width="16" height="16" aria-hidden="true" focusable="false"><path fill="currentColor" d="M8 0C3.58 0 0 3.58 0 8c0 3.54 2.29 6.53 5.47 7.59.4.07.55-.17.55-.38 0-.19-.01-.82-.01-1.49-2.01.37-2.53-.49-2.69-.94-.09-.23-.48-.94-.82-1.13-.28-.15-.68-.52-.01-.53.63-.01 1.08.58 1.23.82.72 1.21 1.87.87 2.33.66.07-.52.28-.87.51-1.07-1.78-.2-3.64-.89-3.64-3.95 0-.87.31-1.59.82-2.15-.08-.2-.36-1.02.08-2.12 0 0 .67-.21 2.2.82.64-.18 1.32-.27 2-.27s1.36.09 2 .27c1.53-1.04 2.2-.82 2.2-.82.44 1.1.16 1.92.08 2.12.51.56.82 1.27.82 2.15 0 3.07-1.87 3.75-3.65 3.95.29.25.54.73.54 1.48 0 1.07-.01 1.93-.01 2.2 0 .21.15.46.55.38A8.01 8.01 0 0 0 16 8c0-4.42-3.58-8-8-8Z"/></svg>
      <span>Source on GitHub</span>
    </a>
  </p>
</footer>"""

# boot.js first - it decides whether the CRT power-on sweep plays at all
# on this load (see that file), so it wants to run before anything else
# has a chance to hold the main thread. Then analytics.js then consent.js,
# always in that order (GA4 must exist before consent.js can call
# window.__dbhqEnableGA on Accept), then the dialog itself. Every page
# that carries this region carries the dialog - GA must never load
# anywhere without it, which has already been got wrong once (see git
# history).
CONSENT = """<script type="module" src="boot.js"></script>
<script type="module" src="analytics.js"></script>
<script type="module" src="consent.js"></script>
<dialog class="consent" data-consent aria-labelledby="consent-title">
  <h2 id="consent-title">CARRIER DETECT</h2>
  <p>
    We would like to count how many people come here to listen, using Google
    Analytics. Cookies are only set if you accept, and the modem answers
    exactly the same either way.
  </p>
  <div class="consent-actions">
    <button type="button" class="dial-button" data-consent-decline>Decline</button>
    <button type="button" class="dial-button" data-consent-accept autofocus>Accept</button>
  </div>
</dialog>"""


def render_region(region: str, nav_id: str | None) -> str:
    if region == "nav":
        return render_nav(nav_id)
    if region == "footer":
        return FOOTER
    if region == "consent":
        return CONSENT
    raise ValueError(f"unknown region {region!r}")


def splice_region(html: str, path: Path, region: str, replacement: str) -> str:
    """Replaces everything between the BEGIN/END marker comments for
    `region` with `replacement` - same mechanism as web/_gen/frames.py's
    own `splice_region`, applied to markup this project authors itself
    rather than markup pulled from another crate. Fails loudly if the
    markers are not found."""
    import re

    begin = f"<!-- BEGIN generated: {region} (web/_gen/pages.py - do not hand-edit) -->"
    end = f"<!-- END generated: {region} -->"
    pattern = re.compile(re.escape(begin) + r".*?" + re.escape(end), re.DOTALL)
    if not pattern.search(html):
        raise SystemExit(
            f"marker pair not found in {path}: {region!r} - expected to find:\n"
            f"  {begin}\n  ...\n  {end}"
        )
    block = f"{begin}\n{replacement}\n{end}"
    return pattern.sub(lambda _m: block, html, count=1)


def main() -> None:
    for filename, config in PAGES.items():
        path = WEB / filename
        html = path.read_text()
        for region in config["regions"]:
            replacement = render_region(region, config["nav_id"])
            html = splice_region(html, path, region, replacement)
        path.write_text(html)
        print(f"wrote {path}")


if __name__ == "__main__":
    main()
