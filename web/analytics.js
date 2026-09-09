// GA4, loaded from a file rather than an inline block.
//
// The page's Content-Security-Policy has no 'unsafe-inline', deliberately -
// an inline script silently killed the entire demo on the live site once
// already. Google's own snippet is inline, so it is rewritten here as an
// ordinary same-origin module and the tag itself is injected. Nothing about
// the measurement changes; only where the code lives.
//
// Stream: property 544327698, "modem", created 9 Sep 2026. It is a second
// data stream in the existing DBHQ property rather than a property of its
// own, so modem and dbhq.uk stay comparable and share one configuration.
const MEASUREMENT_ID = 'G-21S0R1LWLZ';

const tag = document.createElement('script');
tag.async = true;
tag.src = 'https://www.googletagmanager.com/gtag/js?id=' + MEASUREMENT_ID;
document.head.appendChild(tag);

window.dataLayer = window.dataLayer || [];
function gtag() {
  // Deliberately `arguments`, not a rest parameter: gtag reads the live
  // Arguments object itself, and a real array does not behave the same way.
  // eslint-disable-next-line prefer-rest-params
  window.dataLayer.push(arguments);
}
gtag('js', new Date());
gtag('config', MEASUREMENT_ID);
