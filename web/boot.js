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
