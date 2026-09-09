// The consent prompt.
//
// A native <dialog> opened with showModal(), so the browser supplies focus
// move-in, a focus trap, Escape handling and focus return. A hand-rolled
// overlay has none of those.
//
// External rather than inline for the same reason as analytics.js: this
// page's CSP has no 'unsafe-inline', and an inline block has broken this
// page before.
//
// A module, and loaded after analytics.js, so two things are guaranteed
// rather than hoped for: the DOM is parsed by the time this runs (modules
// are deferred, so the dialog is always there to find), and
// window.__dbhqEnableGA is already defined when Accept is clicked, because
// modules execute in document order.
const dlg = document.querySelector('[data-consent]');

if (dlg) {
  let choice = null;
  try {
    choice = localStorage.getItem('dbhq-consent');
  } catch (e) {
    // localStorage throws rather than returning null in some privacy modes.
    // No readable choice means we ask.
  }

  const set = (v) => {
    try {
      localStorage.setItem('dbhq-consent', v);
    } catch (e) {
      // Unwritable storage means we will ask again next visit. Better than
      // failing the click.
    }
    if (dlg.open) dlg.close();
    if (v === 'granted' && typeof window.__dbhqEnableGA === 'function') {
      window.__dbhqEnableGA();
    }
  };

  dlg.querySelector('[data-consent-accept]').addEventListener('click', () => set('granted'));
  dlg.querySelector('[data-consent-decline]').addEventListener('click', () => set('denied'));

  // Only ask if they have not already answered. Escape counts as no answer,
  // so the prompt returns next visit rather than being treated as consent.
  if (choice !== 'granted' && choice !== 'denied') dlg.showModal();
}
