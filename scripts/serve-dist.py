#!/usr/bin/env python3
"""Serves ./dist the way Cloudflare Pages serves it, for the smoke tests.

`python3 -m http.server` is not close enough to be useful here. Pages
resolves `/explained` to `explained.html` and `/demo/` to
`demo/index.html`, and it serves `404.html` with a 404 status for
anything it cannot find. A plain static server 404s every extensionless
path, so tests against the real URLs - which is the whole point of
testing the assembled dist rather than the loose files - fail on the
server rather than on the site.

Deliberately NOT a full emulation. It does path resolution and the 404
page, because those change what the tests see. It does not reproduce
_headers, so the CSP and Cache-Control are absent locally; anything that
depends on those has to be checked against the deployed site instead, and
the deploy workflow already does exactly that.
"""

import sys
from http.server import SimpleHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

DIST = Path(__file__).resolve().parent.parent / "dist"


class PagesHandler(SimpleHTTPRequestHandler):
    def __init__(self, *args, **kwargs):
        super().__init__(*args, directory=str(DIST), **kwargs)

    def translate_path(self, path):
        resolved = super().translate_path(path)
        candidate = Path(resolved)
        if candidate.is_file():
            return resolved
        # /explained -> explained.html
        with_html = candidate.with_suffix(".html")
        if not candidate.suffix and with_html.is_file():
            return str(with_html)
        # /demo/ -> demo/index.html
        index = candidate / "index.html"
        if index.is_file():
            return str(index)
        return resolved

    def send_error(self, code, message=None, explain=None):
        # Pages serves the site's own 404 page, with a 404 status. A test
        # for the 404 page needs to see the real thing, not the stdlib's.
        page = DIST / "404.html"
        if code == 404 and page.is_file():
            body = page.read_bytes()
            self.send_response(404)
            self.send_header("Content-Type", "text/html; charset=utf-8")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            if self.command != "HEAD":
                self.wfile.write(body)
            return
        super().send_error(code, message, explain)

    def log_message(self, *args):
        pass  # quiet: the test reporter is the output that matters


if __name__ == "__main__":
    port = int(sys.argv[1]) if len(sys.argv) > 1 else 4173
    if not DIST.is_dir():
        sys.exit("dist/ not found - run scripts/build-dist.sh first")
    ThreadingHTTPServer(("127.0.0.1", port), PagesHandler).serve_forever()
