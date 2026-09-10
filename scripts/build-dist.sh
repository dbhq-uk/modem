#!/usr/bin/env bash
# Assembles the exact tree that gets uploaded to Cloudflare Pages, into
# ./dist.
#
# One script, used by both the deploy workflow and the smoke tests, so the
# thing CI exercises is the thing that ships. It was inline in
# deploy.yml; the smoke tests need the identical tree (the route
# directories and their injected <base> are part of what they test), and
# two copies of this would drift the first time one changed.
#
# Expects the wasm to have been built already:
#   cargo build --release --target wasm32-unknown-unknown -p modem-wasm --no-default-features
set -euo pipefail

cd "$(dirname "$0")/.."
rm -rf dist
mkdir -p dist/web dist/brand

# The page loads brand/tokens.css and brand/icon.svg from a sibling
# directory, so the upload is the repo root minus everything that is not
# servable. Assembled explicitly rather than uploading the whole tree:
# this is a public repo and the source has no business being served.
cp web/*.html web/*.js web/*.css web/*.png web/*.webp web/*.avif dist/web/ 2>/dev/null || true

# The worklet artifact comes from the build, not from the tree:
# web/modem.wasm is gitignored, so on a fresh checkout it does not exist
# at all. It used to be swept up by the glob above, where `|| true`
# swallowed its absence and shipped a page whose demo 404ed on its own
# payload. No `|| true` here - if this file is missing the build must
# stop.
cp target/wasm32-unknown-unknown/release/modem_wasm.wasm dist/web/modem.wasm

# _headers, _redirects and manifest.json need naming explicitly: none has
# a glob-friendly extension. _headers carries the CSP that permits WASM
# instantiation - without it the demo is blocked at the edge.
cp web/_headers web/_redirects web/manifest.json web/robots.txt web/sitemap.xml dist/web/

# The self-hosted webfonts. Not a glob above because they live in a
# subdirectory, and no `|| true` here: the site's CSP is `font-src
# 'self'`, so if these are missing there is no remote fallback to save
# it - the page silently drops to whatever monospace the OS has, and the
# terminal frames lose their box-drawing alignment. See the @font-face
# block in web/style.css.
mkdir -p dist/web/fonts
cp web/fonts/*.woff2 web/fonts/LICENCE-*.txt dist/web/fonts/
cp brand/tokens.css brand/icon.svg brand/icon-*.png brand/favicon.ico dist/brand/
cp brand/wordmark.svg dist/brand/ 2>/dev/null || true

# The page is served at the root, not at /web/.
mv dist/web/* dist/
rmdir dist/web

# The three routes are real directories, not _redirects rewrites.
# Cloudflare Pages would not honour a 200 rewrite for them: rewriting
# /demo to /index.html canonicalised straight to /, discarding the path
# that page.js reads to decide which route it is. Each directory gets a
# copy of the one index.html with <base href="/"> injected, so its
# relative assets still resolve from the root while location.pathname
# stays /demo/, /originate/ or /receive/. Canonical and og:url are
# rewritten per route as well - a route is real, separately-listed
# content, and its canonical has to say so itself.
python3 - <<'PY'
from pathlib import Path

src = Path('dist/index.html').read_text()
assert '<base' not in src, 'index.html already has a <base>; rework this step'
out = src.replace('<head>', '<head>\n<base href="/">', 1)
canonical = '<link rel="canonical" href="https://modem.dbhq.uk/">'
og_url = '<meta property="og:url" content="https://modem.dbhq.uk/">'
assert out.count(canonical) == 1, 'expected exactly one canonical link to rewrite per route'
assert out.count(og_url) == 1, 'expected exactly one og:url meta to rewrite per route'
for route in ('demo', 'originate', 'receive'):
    d = Path('dist') / route
    d.mkdir(parents=True, exist_ok=True)
    d.joinpath('index.html').write_text(
        out.replace(canonical, f'<link rel="canonical" href="https://modem.dbhq.uk/{route}/">')
           .replace(og_url, f'<meta property="og:url" content="https://modem.dbhq.uk/{route}/">')
    )
    print('wrote', d / 'index.html')
PY

echo "dist assembled:"
ls dist
