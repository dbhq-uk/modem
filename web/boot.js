// The CRT power-on sweep, played once per visit rather than once per
// page.
//
// `.crt-warmup` is a fixed, full-viewport overlay that collapses to a
// bright line and expands back out - the reverse of a tube switching
// off. It is the one authored moment of motion on the site and it is
// worth having when somebody arrives. Playing it again on every internal
// navigation is a different thing entirely: the whole screen flashes
// each time you move between Try, Explained, Prior art and back, which
// reads as the page breaking rather than as a flourish (Dan, 9 Sep
// 2026: "stop the jitter and flicker when changing pages").
//
// So the animation now belongs to a class this file adds, and it is
// added only when `sessionStorage` has no record of the tube already
// having warmed up in this tab. First arrival gets the sweep; every
// navigation after it gets a page that is simply there.
//
// The default in style.css is no animation and `opacity: 0`, which is
// deliberately the *safe* state: if this file never runs - blocked,
// cached oddly, JavaScript off - the visitor gets a page with no sweep,
// not a black overlay with nothing to clear it. That is the same rule
// the overlay's own comment already states, kept true by making the
// scripted path the one that adds motion rather than the one that
// removes it.
//
// `sessionStorage` rather than `localStorage`: a new tab or a new day is
// a new arrival and should get the sweep again. It is wrapped because
// Safari throws on storage access in some private-mode configurations,
// and a decorative flourish must never be able to throw.
const KEY = 'modem:warmed';

try {
  if (!sessionStorage.getItem(KEY)) {
    document.documentElement.classList.add('crt-boot');
    sessionStorage.setItem(KEY, '1');
  }
} catch {
  // Storage unavailable: play it. An extra sweep is a far better
  // failure than none at all on a first visit.
  document.documentElement.classList.add('crt-boot');
}

// -----------------------------------------------------------------------
// Fatal startup reporting.
//
// This lives here rather than in page.js precisely because this file
// imports nothing. A `type="module"` script evaluates its whole import
// graph before its own first line, so listeners registered inside page.js
// cannot report page.js failing to load - and on a phone, a transient
// connection dropping one of the six modules it pulls in is enough to
// leave the entire page inert with no handlers and nothing on screen.
// Registered here, they survive that.
//
// It cannot prevent the failure. What it does is turn "every button does
// nothing" into a line of text naming the file and the line, which on 9
// Sep 2026 was the difference between a five-minute diagnosis and an
// afternoon of one.
//
// The message goes to whichever diagnostic the page has, and never
// overwrites one that already says something - a specific message from
// the code that actually failed beats this generic one.
// -----------------------------------------------------------------------
function reportFatal(what) {
  const el = document.getElementById('demo-diagnostic') || document.getElementById('mic-diagnostic');
  if (!el || el.textContent) return;
  el.textContent = `The demo could not start: ${what}`;
  el.className = 'diagnostic diagnostic--warning';
  el.hidden = false;
}

window.addEventListener('error', (e) => {
  reportFatal(`${e.message} (${(e.filename || '').split('/').pop()}:${e.lineno})`);
});

// Rejections as well as throws: the launcher buttons call `startRoute()`
// without awaiting it, so anything that rejects inside it and is not
// caught there would otherwise go nowhere at all - no console entry a
// visitor sees, no message, just a button that appears to do nothing.
window.addEventListener('unhandledrejection', (e) => {
  reportFatal(e.reason?.message || String(e.reason));
});

// A module that fails to *load* does not reliably reach the
// `window.onerror` handler above: the browser fires `error` at the
// `<script>` element itself, and that event does not bubble. So watch the
// element directly. This is the case Codex named as leaving the page
// "permanently inert until reloaded" - and with the launcher buttons now
// shipping `disabled` (see index.html), it is also the case where they
// would stay disabled with nothing saying why.
//
// Best-effort, and worth being exact about that rather than repeating the
// overclaim this whole change exists to correct: module scripts are
// fetched in parallel and executed in document order, so if page.js's
// fetch has already failed by the time this file runs, its `error` event
// has been and gone and this listener misses it. It catches the common
// case - a fetch still in flight when this executes - and nothing more.
//
// Queried rather than hardcoded to one filename so this keeps working if
// the page gains another module.
for (const script of document.querySelectorAll('script[type="module"][src]')) {
  script.addEventListener('error', () => {
    reportFatal(`${script.getAttribute('src')} did not load - check the connection and reload`);
  });
}
