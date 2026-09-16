// Manners for the nav's "More" disclosure.
//
// The menu itself is a native `<details>`/`<summary>` (see
// web/_gen/pages.py), so it already opens, closes, takes keyboard focus
// and announces its expanded state without any script at all. That is
// deliberate: the nav is the one piece of chrome on every page
// including 404.html, and a nav that needs JavaScript to open is a nav
// that is sometimes shut for good.
//
// What `<details>` has no opinion about is everything a *menu* is
// expected to do once it is open - dismiss on Escape, dismiss when you
// click somewhere else, and not still be hanging open when you come
// back to the page you just navigated away to. Those three things are
// all this file adds. Blocked, cached oddly, or JavaScript off, the
// menu still works; it is only slightly less polite.
const more = document.querySelector('[data-nav-more]');

if (more) {
  const summary = more.querySelector('summary');

  const close = () => { more.open = false; };

  // Escape, from anywhere on the page. Focus goes back to the summary
  // rather than staying wherever it was inside the panel that just
  // disappeared - a keyboard user who dismisses a menu should end up on
  // the control that opened it, not nowhere.
  document.addEventListener('keydown', (event) => {
    if (event.key !== 'Escape' || !more.open) return;
    close();
    summary?.focus();
  });

  // Click-away. `pointerdown` rather than `click` so the menu is gone
  // before whatever was underneath it reacts, and on the document so it
  // catches a press anywhere - including on the page behind the sticky
  // nav.
  document.addEventListener('pointerdown', (event) => {
    if (more.open && !more.contains(event.target)) close();
  });

  // Closing after a link is followed looks pointless - the page is
  // about to be replaced - but it is not. Every page here is a separate
  // document, so following a link inside the panel and then pressing
  // Back can restore this one from the browser's back/forward cache
  // with the menu exactly as it was: open, over the top of the page
  // that just came back. Closing on the way out means it is shut when
  // that happens.
  more.querySelectorAll('a').forEach((link) => {
    link.addEventListener('click', close);
  });

  // And the belt to that braces: bfcache restores a page without
  // re-running any of this, so a menu left open by some route not
  // covered above is still shut on the way back in.
  window.addEventListener('pageshow', close);
}
