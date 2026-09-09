// GA4 with Consent Mode v2, loaded from a file rather than an inline block.
//
// The page's Content-Security-Policy has no 'unsafe-inline', deliberately -
// an inline script silently killed the entire demo on the live site once
// already. Google's own snippet is inline, so it is rewritten here as an
// ordinary same-origin module and the tag itself is injected. Nothing about
// the measurement changes; only where the code lives.
//
// Denied by default. Nothing is loaded and no cookie is set until the
// visitor accepts - see consent.js, which owns the prompt and calls
// __dbhqEnableGA() on accept. This page previously fired GA on load with
// no consent step at all, which set analytics cookies on every visitor
// before being asked. That is a problem beyond this one site: GA4 sets its
// cookie on the shared parent .dbhq.uk, so a cookie dropped here without
// consent is a cookie across the whole estate.
//
// Stream: the estate-wide "DBHQ" stream on property 544327698. It used to
// be G-21S0R1LWLZ, a stream of modem's own, which gave this page a second
// _ga_<id> cookie on that same shared parent - so a visitor arriving from
// dbhq.uk started a fresh session here and the journey between the two was
// lost. One stream also matters for Search Console: that link binds to
// exactly one data stream, so a site on its own ID can never show search
// data in GA4. Split the sites at reporting time with the Hostname
// dimension instead. See dbhq/docs/reference/analytics.md.
const MEASUREMENT_ID = 'G-3H3NFGSX85';

// Never measure a local preview or a Pages branch build - only the real host.
const PROD = location.hostname === 'modem.dbhq.uk';

window.dataLayer = window.dataLayer || [];
function gtag() {
  // Deliberately `arguments`, not a rest parameter: gtag reads the live
  // Arguments object itself, and a real array does not behave the same way.
  // eslint-disable-next-line prefer-rest-params
  window.dataLayer.push(arguments);
}
window.gtag = gtag;

gtag('consent', 'default', {
  ad_storage: 'denied',
  analytics_storage: 'denied',
  ad_user_data: 'denied',
  ad_personalization: 'denied',
});

window.__dbhqEnableGA = function () {
  if (!PROD || window.__gaLoaded) return;
  window.__gaLoaded = true;

  gtag('consent', 'update', { analytics_storage: 'granted' });
  gtag('js', new Date());
  gtag('config', MEASUREMENT_ID);

  const tag = document.createElement('script');
  tag.async = true;
  tag.src = 'https://www.googletagmanager.com/gtag/js?id=' + MEASUREMENT_ID;
  document.head.appendChild(tag);
};

// A visitor who accepted on a previous visit is not asked again. The key is
// shared across *.dbhq.uk, so accepting on dbhq.uk carries over to here.
try {
  if (localStorage.getItem('dbhq-consent') === 'granted') window.__dbhqEnableGA();
} catch (e) {
  // localStorage throws rather than returning null in some privacy modes.
  // No stored choice means no consent, which is already the default.
}
