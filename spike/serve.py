#!/usr/bin/env python3
"""Serve the spike over HTTPS.

AudioWorklet is only exposed in a secure context, so plain HTTP over a LAN
address gives you `ctx.audioWorklet === undefined` and nothing else. The
certificate comes from `tailscale cert`, which issues a genuinely trusted
one for the machine's ts.net name - no click-through warning, which matters
because a browser treats a cert-error origin as insecure.

    sudo tailscale cert <machine>.<tailnet>.ts.net
    python3 serve.py

This is spike scaffolding. The live site gets HTTPS from Cloudflare Pages.
"""

import http.server
import ssl
import sys

HOST = "0.0.0.0"
PORT = 8444
CERT = "/tmp/scentverdict-devops.civet-tegu.ts.net.crt"
KEY = "/tmp/scentverdict-devops.civet-tegu.ts.net.key"


def main() -> int:
    handler = http.server.SimpleHTTPRequestHandler
    httpd = http.server.ThreadingHTTPServer((HOST, PORT), handler)

    ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    ctx.load_cert_chain(CERT, KEY)
    httpd.socket = ctx.wrap_socket(httpd.socket, server_side=True)

    print(f"serving https://scentverdict-devops.civet-tegu.ts.net:{PORT}/", flush=True)
    httpd.serve_forever()
    return 0


if __name__ == "__main__":
    sys.exit(main())
