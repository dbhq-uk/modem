// GA4, ON BY DEFAULT with a simple opt-out, loaded from a file rather than
// an inline block.
//
// The page's Content-Security-Policy has no 'unsafe-inline', deliberately -
// an inline script silently killed the entire demo on the live site once
// already. Google's own snippet is inline, so it is rewritten here as an
// ordinary same-origin module and the tag itself is injected. Nothing about
// the measurement changes; only where the code lives.
//
// OPT-OUT, NOT OPT-IN, SINCE 30 SEP 2026. Every DBHQ site made the same
// switch that day, under the PECR statistical-purposes exception the Data
// (Use and Access) Act 2025 added: analytics may run without prior consent
// when it is used only to improve the site, the output is aggregate, Google
// acts only as a processor, and the visitor gets clear information and a
// simple, free way to object. Until then this page denied every signal and
// loaded nothing until Accept was clicked - and below 1,000 consenting users
// a day GA4 models nobody who declines, so an opt-in gate simply dropped
// everyone who declined or ignored the prompt. The reasoning, what the
// exception requires and the estate-wide pattern are in
// dbhq/docs/reference/analytics.md, section "Analytics notice and opt-out".
//
// Analytics-only is what keeps it inside the exception: the three ad
// signals are denied, and Google Signals and ad personalisation are off in
// the tag. GA4 is not loaded at all for a visitor who opted out, for a
// likely bot, or off the live host. consent.js is the notice; it loads
// after this file and calls window.dbhqAnalytics.
//
// This is a port of the gate in dbhq.uk's web/apex/src/layouts/Base.astro,
// and it should stay the same logic on every site: same cookie, same bot
// check, same opt-out. Only the drawing of the notice differs per site.
//
// Stream: the estate-wide "DBHQ" stream on property 544327698. It used to
// be G-21S0R1LWLZ, a stream of modem's own, which gave this page a second
// _ga_<id> cookie on that same shared parent - so a visitor arriving from
// dbhq.uk started a fresh session here and the journey between the two was
// lost. One stream also matters for Search Console: that link binds to
// exactly one data stream, so a site on its own ID can never show search
// data in GA4. Split the sites at reporting time with the Hostname
// dimension instead.
const MEASUREMENT_ID = 'G-3H3NFGSX85';

// Never measure a local preview or a Pages branch build - only the real host.
const PROD = location.hostname === 'modem.dbhq.uk';

// The choice is a cookie on .dbhq.uk, so opting out on one site opts out on
// every *.dbhq.uk site - the _ga cookie it stops is estate-wide too. A
// localStorage "dbhq-consent" left by the old opt-in prompt is read once and
// carried over: "denied" stays an opt-out. That key was per origin and never
// carried a choice between sites, whatever this file used to claim.
function readChoice() {
  const m = document.cookie.match(/(?:^|; )dbhq_analytics=(on|off)(?:;|$)/);
  if (m) return m[1];
  try {
    const old = localStorage.getItem('dbhq-consent');
    if (old === 'denied') return 'off';
    if (old === 'granted') return 'on';
  } catch (e) {
    // localStorage throws rather than returning null in some privacy modes.
    // No readable old choice means no choice.
  }
  return null;
}

function writeChoice(v) {
  let c = 'dbhq_analytics=' + v + '; Max-Age=31536000; Path=/; SameSite=Lax; Secure';
  // Host-only anywhere outside dbhq.uk, such as a pages.dev branch build.
  if (/(^|\.)dbhq\.uk$/.test(location.hostname)) c += '; Domain=dbhq.uk';
  document.cookie = c;
  try {
    localStorage.removeItem('dbhq-consent');
  } catch (e) {
    // Nothing to remove if storage is unreadable.
  }
}

// Bots that run JavaScript, and scrapers rotating desktop Chrome or Firefox
// about two years stale. Mobile, Win7/8 and Firefox ESR are exempt. Kept
// identical to ScentVerdict's svAnalytics.likelyBot and to dbhq.uk's copy -
// change all three or none.
function likelyBot() {
  try {
    if (navigator.webdriver) return true;
    var ua = navigator.userAgent || "";
    if (/bot|crawl|spider|headless/i.test(ua)) return true;
    if (/Android|Mobile|CrOS/.test(ua) || !/Windows NT 10\.0|Macintosh|X11/.test(ua)) return false;
    var n = Math.max(0, Math.floor((Date.now() - Date.UTC(2025, 8, 2)) / 2592e6));
    var c = /Chrome\/(\d+)\./.exec(ua);
    if (c) return +c[1] < 140 + n - 24;
    var f = /Firefox\/(\d+)\./.exec(ua);
    if (f) return [115, 128, 140, 153].indexOf(+f[1]) < 0 && +f[1] < 142 + n - 24;
  } catch (e) {}
  return false;
}

// GA4 sets _ga on the highest domain it can (.dbhq.uk), so expire the
// cookies on this host and on every parent domain.
function deleteGaCookies() {
  const parts = location.hostname.split('.');
  document.cookie.split('; ').forEach((c) => {
    const name = c.split('=')[0];
    if (name === '_ga' || name.indexOf('_ga_') === 0) {
      const expired = name + '=; expires=Thu, 01 Jan 1970 00:00:00 GMT; path=/';
      document.cookie = expired;
      for (let i = 0; i < parts.length - 1; i++) {
        document.cookie = expired + '; domain=.' + parts.slice(i).join('.');
      }
    }
  });
}

window.dataLayer = window.dataLayer || [];
function gtag() {
  // Deliberately `arguments`, not a rest parameter: gtag reads the live
  // Arguments object itself, and a real array does not behave the same way.
  // eslint-disable-next-line prefer-rest-params
  window.dataLayer.push(arguments);
}
window.gtag = gtag;

const bot = likelyBot();
let loaded = false;

function load() {
  if (!PROD || bot || loaded) return;
  loaded = true;
  gtag('consent', 'default', {
    analytics_storage: 'granted',
    ad_storage: 'denied',
    ad_user_data: 'denied',
    ad_personalization: 'denied',
  });
  gtag('js', new Date());
  gtag('config', MEASUREMENT_ID, {
    allow_google_signals: false,
    allow_ad_personalization_signals: false,
  });
  const tag = document.createElement('script');
  tag.async = true;
  tag.src = 'https://www.googletagmanager.com/gtag/js?id=' + MEASUREMENT_ID;
  document.head.appendChild(tag);
}

window.dbhqAnalytics = {
  choice: readChoice,
  keepOn() {
    writeChoice('on');
    window['ga-disable-' + MEASUREMENT_ID] = false;
    if (loaded) gtag('consent', 'update', { analytics_storage: 'granted' });
    load();
  },
  // Denied consent alone still lets GA4 send cookieless pings, including the
  // user_engagement hit it flushes when the page is left. Google's ga-disable
  // flag stops every hit from this page; later pages do not load GA4 at all.
  optOut() {
    writeChoice('off');
    window['ga-disable-' + MEASUREMENT_ID] = true;
    if (loaded) gtag('consent', 'update', { analytics_storage: 'denied' });
    deleteGaCookies();
  },
};

const choice = readChoice();
// An old localStorage choice is written onto the estate cookie on the first
// visit that reads it, and the key removed.
if (choice && !/(?:^|; )dbhq_analytics=/.test(document.cookie)) writeChoice(choice);
if (choice !== 'off') load();
