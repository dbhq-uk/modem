#!/usr/bin/env python3
"""Generates the three shared-chrome regions repeated across modem's seven
pages: the primary nav, the footer, and the consent/analytics block.

Edit this, then regenerate - never hand-edit the three marked regions in
web/index.html, web/explained.html, web/research.html, web/debugging.html,
web/downloads.html,
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
the actual content of each of the seven pages, and templating it away
would be the opposite of "seven real pages, each with its own title and
h1". Only the chrome every page shares - the nav (three links, a "More"
disclosure holding four, and the script that makes the disclosure
well-mannered), the footer's byline/groups, and the consent dialog plus
its two script tags - is generated.
"""

from pathlib import Path

HERE = Path(__file__).resolve().parent
WEB = HERE.parent

# THE NAV IS THREE ITEMS AND A DISCLOSURE, not seven items in a row
# (Dan, 16 Sep 2026: "i feel we probaby need to dropodwn on the nav to
# make some rooms"). Seven links at the nav's own small type ran to the
# full width of a phone with nothing left over, and the seventh arrived
# the same day the sixth did - so the next page added would have been
# the one that broke it.
#
# The split is by what the visitor came to do, not by subject. Hear it,
# Explained and Downloads are the three things somebody arrives wanting:
# hear the thing, understand it, get it. Research, Debugging, Projects
# and About are all secondary reading that nobody lands here looking for,
# so they sit behind one disclosure rather than costing four slots.
#
# Each tuple is id, label, href. The id is the internal key for
# aria-current and never changes with the label.
NAV_ITEMS = (
    # "Hear it", not "Try" (Dan, 9 Sep 2026). Try names an effort the
    # visitor has to make; this names what they get, and it says the same
    # thing the page's own h1 does - "Hear the dial-up sound, live". The
    # id stays `try` because it is the internal key, not the label, and
    # renaming it would move every aria-current mapping below for
    # nothing.
    ("try", "Hear it", "/"),
    ("explained", "Explained", "/explained"),
    ("downloads", "Downloads", "/downloads"),
)

# What sits behind "More". Order runs from the pages about this project
# outward to the pages about who made it: what was found out (Research,
# Debugging), then the rest of the estate (Projects), then the colophon
# (About).
NAV_MORE = (
    ("research", "Research", "/research"),
    # Added 16 Sep 2026: the account of fixing the acoustic mode, after
    # three confident wrong answers. It sits next to Research because it
    # is the same kind of page - what was found out, rather than what the
    # thing does.
    ("debugging", "Debugging", "/debugging"),
    # "Projects", not "DBHQ" (Dan, 16 Sep 2026), reversing the 10 Sep
    # call. "DBHQ" named the thing rather than describing the list, which
    # was the right instinct for a top-level item sat between Downloads
    # and About - but inside a disclosure the label's job changes. The
    # four items under "More" are read as a list, and three of them are
    # page names while the fourth was an organisation name; it read as
    # the odd one out rather than as the specific one. The id and the URL
    # stay `projects` regardless: the id is the internal key for
    # aria-current, and changing the URL would break every link already
    # pointing at /projects, including the sitemap and the sibling sites'
    # own footers.
    ("projects", "Projects", "/projects"),
    ("about", "About", "/about"),
)

# The label on the disclosure itself. Not a page, so it has no id and
# never takes aria-current - but it does get a marker class when the
# current page is one of the four behind it, or there would be no sign
# anywhere in the chrome of where you are.
MORE_LABEL = "More"

# Which page each file is, for aria-current="page" - and which of the
# three regions each file actually carries. 404.html gets a nav and a
# footer (so it is not a dead end) but no consent dialog: it is
# noindexed and carries no analytics of its own to gate.
PAGES = {
    "index.html": {"nav_id": "try", "regions": ("nav", "footer", "consent")},
    "explained.html": {"nav_id": "explained", "regions": ("nav", "footer", "consent")},
    "research.html": {"nav_id": "research", "regions": ("nav", "footer", "consent")},
    "debugging.html": {"nav_id": "debugging", "regions": ("nav", "footer", "consent")},
    "downloads.html": {"nav_id": "downloads", "regions": ("nav", "footer", "consent")},
    "projects.html": {"nav_id": "projects", "regions": ("nav", "footer", "consent")},
    "about.html": {"nav_id": "about", "regions": ("nav", "footer", "consent")},
    "404.html": {"nav_id": None, "regions": ("nav", "footer")},
}


# A chevron, so the disclosure looks like one before it is touched. Same
# 24-unit stroked grid as every other icon on the site (see
# web/_gen/icons.py), drawn here rather than fetched because this is the
# only place it appears and a request for eleven bytes of path is not
# worth a file.
CHEVRON = (
    '<svg class="site-nav__chevron" viewBox="0 0 24 24" fill="none" '
    'stroke="currentColor" stroke-width="2" stroke-linecap="round" '
    'stroke-linejoin="round" aria-hidden="true" focusable="false">'
    '<path d="m6 9 6 6 6-6"/></svg>'
)


def render_nav(current_id: str | None) -> str:
    """The sticky primary nav: three links and a "More" disclosure holding
    four more. `current_id` is None on 404.html, where none of the seven
    is "the current page" - a 404 is not one of them.

    The disclosure is a native `<details>`/`<summary>`, so it opens, takes
    keyboard focus and announces its state with no JavaScript at all -
    web/nav.js only adds the three conveniences the element has no
    opinion about (Escape, click-away, and closing after a link is
    followed). With the script blocked or still loading the menu is fully
    usable; without the element it would not be."""

    def link(item_id: str, label: str, href: str, indent: str) -> str:
        current = ' aria-current="page"' if item_id == current_id else ""
        return f'{indent}<a class="site-nav__link" href="{href}"{current}>{label}</a>'

    rows = [link(*item, "  ") for item in NAV_ITEMS]

    # The marker on the summary when the open page is one of the four
    # inside. It is a class rather than aria-current: only one element in
    # a nav may be the current page, and that is the link itself, which
    # is in the markup whether the disclosure is open or shut.
    inside = any(item_id == current_id for item_id, _, _ in NAV_MORE)
    summary_class = "site-nav__summary"
    if inside:
        summary_class += " site-nav__summary--current"

    more = [link(*item, "      ") for item in NAV_MORE]
    body = "\n".join(more)
    rows.append(
        f'  <details class="site-nav__more" data-nav-more>\n'
        f'    <summary class="{summary_class}">{MORE_LABEL}{CHEVRON}</summary>\n'
        f'    <div class="site-nav__panel">\n{body}\n    </div>\n'
        f'  </details>'
    )

    links = "\n".join(rows)
    return (
        f'<nav class="site-nav" aria-label="Primary">\n{links}\n</nav>\n'
        f'<script type="module" src="nav.js"></script>'
    )


# The footer: the required "a DBHQ experiment by..." byline, then one
# link to the source with GitHub's own mark beside it. Identical, word
# for word, on all seven pages and on 404.html.
#
# The byline is short on purpose. It used to carry the crate list and
# "lives on GitHub" as well, which at the footer's type ran to three
# centred lines of very uneven length - and centred text cannot be made
# to flow, because you do not control where it breaks (Dan, 10 Sep 2026:
# "balance the footer - work out how to make it flow nicely").
#
# Shortening it rather than restyling it, because the missing half was
# duplication in the first place: "lives on GitHub" is the button
# directly underneath, and the full crate list is a sentence on /about
# ("Every crate in it - modem-core, modem-audio, modem-tui, modem-wasm -
# and this page are open source under the MIT licence"). Nothing is lost
# from the site; one line stops saying what the line below it says.
#
# What is left is short enough not to wrap at all above a phone, so there
# is no rag to balance.
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
  <p class="endorsement__byline">a <a href="https://dbhq.uk">DBHQ</a> experiment by <a href="https://dbhq.uk">Daniel Grimes</a>. MIT licensed.</p>

  <p class="endorsement__source">
    <a class="endorsement__github" href="https://github.com/dbhq-uk/modem" rel="noopener">
      <svg class="endorsement__github-mark" viewBox="0 0 16 16" width="16" height="16" aria-hidden="true" focusable="false"><path fill="currentColor" d="M8 0C3.58 0 0 3.58 0 8c0 3.54 2.29 6.53 5.47 7.59.4.07.55-.17.55-.38 0-.19-.01-.82-.01-1.49-2.01.37-2.53-.49-2.69-.94-.09-.23-.48-.94-.82-1.13-.28-.15-.68-.52-.01-.53.63-.01 1.08.58 1.23.82.72 1.21 1.87.87 2.33.66.07-.52.28-.87.51-1.07-1.78-.2-3.64-.89-3.64-3.95 0-.87.31-1.59.82-2.15-.08-.2-.36-1.02.08-2.12 0 0 .67-.21 2.2.82.64-.18 1.32-.27 2-.27s1.36.09 2 .27c1.53-1.04 2.2-.82 2.2-.82.44 1.1.16 1.92.08 2.12.51.56.82 1.27.82 2.15 0 3.07-1.87 3.75-3.65 3.95.29.25.54.73.54 1.48 0 1.07-.01 1.93-.01 2.2 0 .21.15.46.55.38A8.01 8.01 0 0 0 16 8c0-4.42-3.58-8-8-8Z"/></svg>
      <span>Source on GitHub</span>
    </a>
  </p>
</footer>"""

# The two dialog buttons carry their own icons rather than going through
# web/_gen/icons.py: that script matches on a button's visible text, and
# "Decline"/"Accept" are generated here, so it would never see them in a
# source file to rewrite. A cross and a tick - the one pair on the site
# where the two choices are opposites and the glyph says so faster than
# the word.
#
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
    <button type="button" class="dial-button" data-consent-decline><svg class="dial-button__icon" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true" focusable="false"><path d="M18 6 6 18"/><path d="m6 6 12 12"/></svg><span>Decline</span></button>
    <button type="button" class="dial-button" data-consent-accept autofocus><svg class="dial-button__icon" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true" focusable="false"><path d="M20 6 9 17l-5-5"/></svg><span>Accept</span></button>
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
