// The analytics notice.
//
// Analytics is on by default under the PECR statistical-purposes exception,
// which needs clear information and a simple, free way to object - not prior
// consent. So this informs and offers "Opt out"; it does not ask. It
// replaced a modal Accept/Decline <dialog> on 30 Sep 2026 (see analytics.js
// for why, and dbhq/docs/reference/analytics.md for the estate-wide pattern).
//
// NON-MODAL ON PURPOSE. There is nothing to agree to before reading, so it
// must not block the page: no showModal(), no focus trap, no backdrop - the
// demo underneath stays usable with the notice up. "Opt out" is the same
// size, colour and weight as "OK", so objecting is no harder than carrying
// on. The footer's "Cookie settings" reopens it.
//
// External rather than inline for the same reason as analytics.js: this
// page's CSP has no 'unsafe-inline', and an inline block has broken this
// page before.
//
// A module, and loaded after analytics.js, so two things are guaranteed
// rather than hoped for: the DOM is parsed by the time this runs (modules
// are deferred, so the notice is always there to find), and
// window.dbhqAnalytics is already defined, because modules execute in
// document order.
const box = document.querySelector('[data-analytics-notice]');
const api = window.dbhqAnalytics;

if (box && api) {
  const status = box.querySelector('[data-analytics-status]');
  const on = box.querySelector('[data-analytics-on]');
  const off = box.querySelector('[data-analytics-off]');
  // The label is a <span> inside the button, beside its icon - setting the
  // button's own textContent would delete the icon.
  const onLabel = on.querySelector('span');
  let returnTo = null;

  const show = (reopened) => {
    const choice = api.choice();
    status.hidden = !reopened;
    status.textContent = choice === 'off' ? 'Analytics is off in this browser.' : 'Analytics is on in this browser.';
    onLabel.textContent = choice === 'off' ? 'Turn back on' : 'OK';
    box.hidden = false;
    if (reopened) box.focus();
  };

  const hide = () => {
    box.hidden = true;
    if (returnTo) {
      returnTo.focus();
      returnTo = null;
    }
  };

  on.addEventListener('click', () => {
    api.keepOn();
    hide();
  });
  off.addEventListener('click', () => {
    api.optOut();
    hide();
  });
  // Escape closes a reopened notice without changing anything. A first-visit
  // notice stays until answered, so it is not dismissed by a stray key.
  box.addEventListener('keydown', (e) => {
    if (e.key === 'Escape' && api.choice()) hide();
  });

  // "Cookie settings" controls are hidden until this script runs, so a
  // visitor without JavaScript - who gets no analytics either - is not shown
  // a button that does nothing. 404.html carries the footer but not this
  // script, so its control stays hidden: that page loads no analytics.
  document.querySelectorAll('[data-analytics-settings]').forEach((b) => {
    b.hidden = false;
    b.addEventListener('click', () => {
      returnTo = b;
      show(true);
    });
  });

  if (!api.choice()) show(false);
}
