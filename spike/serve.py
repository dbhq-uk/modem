#!/usr/bin/env python3
"""Serve the spike over HTTPS.

AudioWorklet is only exposed in a secure context, so plain HTTP over a LAN
address gives you `ctx.audioWorklet === undefined` and nothing else. The
certificate comes from `tailscale cert`, which issues a genuinely trusted
one for the machine's ts.net name - no click-through warning, which matters
because a browser treats a cert-error origin as insecure.

    sudo tailscale cert "$(tailscale status --json | jq -r .Self.DNSName | sed 's/\\.$//')"
    MODEM_SPIKE_HOST=<that same name> python3 serve.py

If MODEM_SPIKE_HOST is unset the hostname is read from `tailscale status`,
so on a machine with Tailscale running this needs no arguments at all.

This is spike scaffolding. The live site gets HTTPS from Cloudflare Pages.
"""

import http.server
import json
import os
import ssl
import subprocess
import sys

HOST = "0.0.0.0"
PORT = 8444


def tailscale_hostname() -> str:
    """The machine's own ts.net name, without the trailing dot."""
    override = os.environ.get("MODEM_SPIKE_HOST")
    if override:
        return override
    out = subprocess.run(
        ["tailscale", "status", "--json"], capture_output=True, text=True, check=True
    )
    return json.loads(out.stdout)["Self"]["DNSName"].rstrip(".")


# The three routes web/_redirects rewrites to index.html on Cloudflare
# Pages (a 200 rewrite, not a redirect - the URL stays put, only the
# served bytes change). SimpleHTTPRequestHandler has no equivalent, so a
# local `GET /demo` 404s unless this handler does the same rewrite
# itself - and the whole point of page.js's routing is what happens on a
# fresh load of exactly these paths, which is untestable locally without
# it. Kept as a literal list, not derived from _redirects, since a
# three-line file is not worth a parser: if _redirects ever grows a
# fourth route, this needs the same line added by hand.
REWRITE_TO_INDEX = {"/demo", "/originate", "/receive"}


class RewritingHandler(http.server.SimpleHTTPRequestHandler):
    def do_GET(self):
        path = self.path.split("?", 1)[0].split("#", 1)[0]
        if path in REWRITE_TO_INDEX:
            self.path = "/index.html"
        super().do_GET()


def main() -> int:
    try:
        host = tailscale_hostname()
    except Exception as err:
        print(f"could not determine the ts.net hostname: {err}", file=sys.stderr)
        print("set MODEM_SPIKE_HOST to it explicitly", file=sys.stderr)
        return 1

    cert = f"/tmp/{host}.crt"
    key = f"/tmp/{host}.key"
    if not (os.path.exists(cert) and os.path.exists(key)):
        print(f"no certificate at {cert}", file=sys.stderr)
        print(f"run: sudo tailscale cert {host}", file=sys.stderr)
        return 1

    handler = RewritingHandler
    httpd = http.server.ThreadingHTTPServer((HOST, PORT), handler)

    ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    ctx.load_cert_chain(cert, key)
    httpd.socket = ctx.wrap_socket(httpd.socket, server_side=True)

    print(f"serving https://{host}:{PORT}/", flush=True)
    httpd.serve_forever()
    return 0


if __name__ == "__main__":
    sys.exit(main())
