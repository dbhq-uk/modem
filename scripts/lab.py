#!/usr/bin/env python3
"""Operator side of the acoustic lab: set the plan, read the reports.

The devices poll `https://modem.dbhq.uk/api/lab/plan` and post what they
measure. Both sides of that live in Cloudflare KV. This talks to KV
directly over the REST API, so driving two devices in a room is a command
here, not a deploy.

    source ~/.dbhq/env.sh

    scripts/lab.py devices                 # who has checked in
    scripts/lab.py plan --file step.json   # set what they should do
    scripts/lab.py idle                    # stop them
    scripts/lab.py reports                 # everything they have posted
    scripts/lab.py reports --device phone --last 3
    scripts/lab.py clear                   # bin the reports

The plan is one JSON object:

    {
      "note": "self-leak, phone",
      "devices": {
        "phone":  {"op": "toneAndMeasure", "hz": 2225, "seconds": 4, "gain": 0.5},
        "laptop": {"op": "measure", "seconds": 4, "label": "hearing the phone"}
      },
      "default": {"op": "idle"}
    }

`revision` is added automatically and bumped on every set, because a
device runs a step once per (revision, step) pair - so re-issuing the
same step is a bump, not a special case.

Ops a device understands (see web/lab.js): `idle`, `reload`, `tone`,
`measure`, `toneAndMeasure`, and `modem` - the last of which runs the
real ModemEndpoint and streams its whole timeline back.
"""

import argparse
import json
import os
import sys
import urllib.error
import urllib.parse
import urllib.request

ACCOUNT = os.environ.get("CLOUDFLARE_ACCOUNT_ID")
TOKEN = os.environ.get("CLOUDFLARE_API_TOKEN")
# Created 15 Sep 2026 and bound to the Pages project as `LAB` on both the
# production and preview environments. The Pages project's Terraform
# deliberately ignores deployment_configs (see infra/main.tf), so the
# binding is set out of band by design rather than by omission.
NAMESPACE = "bcd2460c90cd46ccb283975ec0aac7d3"
BASE = f"https://api.cloudflare.com/client/v4/accounts/{ACCOUNT}/storage/kv/namespaces/{NAMESPACE}"


def require_env():
    if not ACCOUNT or not TOKEN:
        sys.exit("CLOUDFLARE_ACCOUNT_ID / CLOUDFLARE_API_TOKEN not set - run: source ~/.dbhq/env.sh")


def call(method, path, data=None, raw=False):
    req = urllib.request.Request(f"{BASE}{path}", method=method)
    req.add_header("Authorization", f"Bearer {TOKEN}")
    body = None
    if data is not None:
        body = data.encode() if isinstance(data, str) else json.dumps(data).encode()
        req.add_header("Content-Type", "application/json" if not isinstance(data, str) else "text/plain")
    try:
        with urllib.request.urlopen(req, body) as res:
            text = res.read().decode()
    except urllib.error.HTTPError as err:
        sys.exit(f"{method} {path} -> {err.code}: {err.read().decode()[:400]}")
    if raw:
        return text
    return json.loads(text)


def list_keys(prefix):
    keys, cursor = [], ""
    while True:
        q = urllib.parse.urlencode({"prefix": prefix, "limit": 1000, **({"cursor": cursor} if cursor else {})})
        res = call("GET", f"/keys?{q}")
        keys.extend(k["name"] for k in res.get("result", []))
        cursor = (res.get("result_info") or {}).get("cursor") or ""
        if not cursor:
            return keys


def get_value(key):
    return call("GET", f"/values/{urllib.parse.quote(key, safe='')}", raw=True)


def put_value(key, value):
    # The bulk endpoint takes JSON and avoids the multipart form the
    # single-key PUT wants.
    return call("PUT", "/bulk", [{"key": key, "value": value}])


def current_plan():
    try:
        return json.loads(get_value("plan"))
    except SystemExit:
        return {"revision": 0}
    except json.JSONDecodeError:
        return {"revision": 0}


def set_plan(plan):
    plan = dict(plan)
    plan["revision"] = int(current_plan().get("revision", 0)) + 1
    plan.setdefault("default", {"op": "idle"})
    put_value("plan", json.dumps(plan))
    print(f"revision {plan['revision']}: {plan.get('note', '')}")
    for name, step in (plan.get("devices") or {}).items():
        print(f"  {name}: {json.dumps(step)}")
    if not plan.get("devices"):
        print(f"  (all): {json.dumps(plan['default'])}")


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd", required=True)
    sub.add_parser("devices")
    p = sub.add_parser("plan")
    p.add_argument("--file", required=True, help="JSON file, or - for stdin")
    sub.add_parser("idle")
    sub.add_parser("show")
    r = sub.add_parser("reports")
    r.add_argument("--device")
    r.add_argument("--last", type=int, default=10)
    r.add_argument("--full", action="store_true", help="whole value, not a summary")
    sub.add_parser("clear")
    args = ap.parse_args()
    require_env()

    if args.cmd == "devices":
        for key in sorted(list_keys("device:")):
            d = json.loads(get_value(key))
            mic = d.get("microphone") or {}
            print(f"{key[7:]:12} {d.get('at')}  {d.get('contextSampleRate')} Hz  "
                  f"session={d.get('audioSession')}  "
                  f"ec={mic.get('echoCancellation')} ns={mic.get('noiseSuppression')} agc={mic.get('autoGainControl')}")
            print(f"{'':12} {(d.get('userAgent') or '')[:100]}")
        return

    if args.cmd == "show":
        print(json.dumps(current_plan(), indent=2))
        return

    if args.cmd == "idle":
        set_plan({"note": "idle", "devices": {}, "default": {"op": "idle"}})
        return

    if args.cmd == "plan":
        text = sys.stdin.read() if args.file == "-" else open(args.file).read()
        set_plan(json.loads(text))
        return

    if args.cmd == "reports":
        keys = sorted(list_keys("report:"))
        if args.device:
            keys = [k for k in keys if f":{args.device}:" in k]
        for key in keys[-args.last:]:
            value = json.loads(get_value(key))
            print(f"\n=== {key}")
            if args.full:
                print(json.dumps(value, indent=2))
            else:
                print(json.dumps(value.get("data", value))[:4000])
        if not keys:
            print("no reports yet")
        return

    if args.cmd == "clear":
        keys = list_keys("report:")
        if not keys:
            print("nothing to clear")
            return
        call("POST", "/bulk/delete", keys)
        print(f"deleted {len(keys)} reports")
        return


if __name__ == "__main__":
    main()
