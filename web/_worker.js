// The acoustic lab's collector, and nothing else.
//
// Two real devices in a room load /lab, register here, poll for an
// instruction, carry it out, and post what they measured. This is the
// far end of that: it stores reports in KV and hands back the current
// plan. The plan itself is written straight into KV from the operator's
// machine over Cloudflare's REST API, so changing what the devices do is
// one API call and takes effect on their next poll - no deploy.
//
// # Why this exists at all
//
// Every round of fixing the two-device acoustic mode so far has been
// reasoning against a model, and the model's central figure - how much
// louder a device's own loudspeaker is in its own microphone than the
// far device is - was inverse-square arithmetic, never a measurement.
// That produced a confident wrong answer twice. This closes the loop
// with real hardware in a real room.
//
// # Why `_worker.js` with `_routes.json` rather than a `functions/` dir
//
// `_routes.json` restricts this worker to `/api/lab/*`. Every other path
// on the site is served by the platform as a static asset and never
// enters this file, so a mistake in here cannot take the site down -
// only the lab. That property is worth more than the tidier file layout
// of directory-based routing, on a site that is live.
//
// # Abuse surface, and what bounds it
//
// The write endpoints take a shared key. This repository is public, so
// the existence and shape of this endpoint are public too, and an
// unauthenticated write on a free-tier KV namespace is a daily write
// quota somebody else can spend. The key does not live here - it is
// written into KV as `labkey` by the operator, and compared against
// `?key=` on the request.
//
// This is a speed bump, not authentication: the key travels in a URL
// that a phone and a laptop both hold in their address bars, and
// anything a browser can hold, a browser can leak. It stops casual
// abuse, which is all that is actually on the table here.
//
// Bounded regardless of the key: 64 KB a request, a 7-day TTL on every
// key written, and nothing is ever read back out by this worker. The
// operator reads KV directly over the REST API. There is no endpoint
// here that returns stored data, so the worst anyone can do is write
// rubbish into a namespace nobody serves.

const MAX_BODY = 64 * 1024;
/// Reports expire on their own. A calibration run is interesting for
/// hours, not for ever, and an unauthenticated write endpoint should not
/// accumulate anything permanently.
const TTL_SECONDS = 7 * 24 * 60 * 60;

const json = (obj, status = 200) =>
  new Response(JSON.stringify(obj), {
    status,
    headers: {
      'content-type': 'application/json',
      'cache-control': 'no-store',
      // The lab page is same-origin, so this is not needed for it to
      // work. It is here so the page can also be opened from a local
      // file or a different host while iterating.
      'access-control-allow-origin': '*',
      'access-control-allow-headers': 'content-type',
      'access-control-allow-methods': 'GET,POST,OPTIONS',
    },
  });

async function readBody(request) {
  const length = Number(request.headers.get('content-length') || 0);
  if (length > MAX_BODY) return null;
  const text = await request.text();
  if (text.length > MAX_BODY) return null;
  try {
    return JSON.parse(text);
  } catch {
    return null;
  }
}

/** Trimmed hard: these land in KV key names. */
const clean = (v, n = 40) => String(v ?? 'unknown').replace(/[^\w.-]/g, '').slice(0, n) || 'unknown';

export default {
  async fetch(request, env) {
    const url = new URL(request.url);

    if (!url.pathname.startsWith('/api/lab/')) {
      // Unreachable while _routes.json is correct. Kept so that a
      // mistake there degrades to "the site still works" rather than
      // "the worker answers everything with a 404".
      return env.ASSETS.fetch(request);
    }
    if (request.method === 'OPTIONS') return json({ ok: true });
    if (!env.LAB) return json({ error: 'LAB namespace is not bound' }, 500);

    // What the devices should be doing now. Written by the operator
    // directly into KV; this only reads it.
    if (url.pathname === '/api/lab/plan') {
      const device = clean(url.searchParams.get('device'));
      const raw = await env.LAB.get('plan');
      let plan;
      try {
        plan = raw ? JSON.parse(raw) : null;
      } catch {
        plan = null;
      }
      if (!plan) plan = { revision: 0, note: 'no plan set', devices: {}, default: { op: 'idle' } };
      const step = (plan.devices && plan.devices[device]) || plan.default || { op: 'idle' };
      return json({ revision: plan.revision ?? 0, note: plan.note ?? '', device, step });
    }

    if (request.method !== 'POST') return json({ error: 'method not allowed' }, 405);

    // Compared only if one has been set, so the lab still works the
    // moment the namespace is empty - a missing key locks nobody out of
    // their own instrument.
    const expected = await env.LAB.get('labkey');
    if (expected && url.searchParams.get('key') !== expected) {
      return json({ error: 'bad or missing key' }, 403);
    }

    const body = await readBody(request);
    if (body === null) return json({ error: 'body must be JSON and under 64 KB' }, 400);
    const device = clean(body.device);
    const now = new Date().toISOString();

    if (url.pathname === '/api/lab/hello') {
      await env.LAB.put(
        `device:${device}`,
        JSON.stringify({ ...body, at: now, cf: request.cf?.colo ?? null }),
        { expirationTtl: TTL_SECONDS },
      );
      return json({ ok: true, device, at: now });
    }

    if (url.pathname === '/api/lab/report') {
      // Sortable key: the operator lists by prefix and gets them in
      // order without reading a single value.
      const key = `report:${now}:${device}:${crypto.randomUUID().slice(0, 8)}`;
      await env.LAB.put(key, JSON.stringify({ ...body, at: now }), {
        expirationTtl: TTL_SECONDS,
      });
      return json({ ok: true, key });
    }

    return json({ error: 'no such endpoint' }, 404);
  },
};
