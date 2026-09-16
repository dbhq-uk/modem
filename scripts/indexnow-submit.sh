#!/usr/bin/env bash
# Submit modem.dbhq.uk's live URLs to IndexNow, which tells Bing, Yandex
# and the other participants about a change immediately instead of
# waiting for them to crawl.
#
# Self-hosted, the same way dbhq.uk does it: the key below is public and
# is also served at https://modem.dbhq.uk/<key>.txt (web/<key>.txt). It
# is an ownership proof, not a secret - anyone can read it, and that is
# the point. Only somebody who can put that file on this host can use it.
#
# Run AFTER the deploy and the edge is serving, never before: IndexNow
# fetches the key file to verify, and submitting URLs that 404 is worse
# than not submitting at all. The deploy workflow does this for you; this
# script is for a manual fallback deploy.
#
# The URL list comes from the live sitemap rather than being listed here,
# so it cannot drift from what the site actually publishes - that is how
# a seventh page gets submitted without anybody remembering to add it.
set -euo pipefail

HOST="modem.dbhq.uk"
KEY="a736f11dc15c3db89492be80e943e323"
KEY_LOCATION="https://${HOST}/${KEY}.txt"
SITEMAP="https://${HOST}/sitemap.xml"

if ! curl -fsS "${KEY_LOCATION}" | grep -qx "${KEY}"; then
  echo "ERROR: key file is not live at ${KEY_LOCATION} - deploy first." >&2
  exit 1
fi

mapfile -t URLS < <(curl -fsS "${SITEMAP}" | grep -oE '<loc>[^<]+' | sed 's/<loc>//')
if [ "${#URLS[@]}" -eq 0 ]; then
  echo "ERROR: no URLs found in ${SITEMAP}" >&2
  exit 1
fi
echo "Submitting ${#URLS[@]} URLs to IndexNow..."

BODY=$(printf '%s\n' "${URLS[@]}" | python3 -c "
import sys, json
urls = [u.strip() for u in sys.stdin if u.strip()]
print(json.dumps({
  'host': '${HOST}',
  'key': '${KEY}',
  'keyLocation': '${KEY_LOCATION}',
  'urlList': urls,
}))
")

CODE=$(curl -s -o /tmp/indexnow-response -w "%{http_code}" \
  -X POST "https://api.indexnow.org/IndexNow" \
  -H "Content-Type: application/json; charset=utf-8" \
  --data "${BODY}")

echo "IndexNow -> ${CODE}"
cat /tmp/indexnow-response 2>/dev/null || true
echo
# 200 accepted, 202 accepted but the key is still being validated. Both fine.
case "${CODE}" in
  200|202) exit 0 ;;
  *) echo "IndexNow rejected the submission" >&2; exit 1 ;;
esac
