// End-to-end smoke tests for modem.dbhq.uk, run against the exact tree
// the deploy uploads (scripts/build-dist.sh), in a real browser.
//
// Every test here is pinned to something that actually shipped broken and
// reached Dan rather than CI, over 9-10 Sep 2026. The existing CI checks
// all passed through every one of them, because they check that files
// exist and that URLs return 200 - and each of these bugs served a
// perfect 200 while the page did nothing.
//
//   - `new Waterfall(null)` at module scope killed page.js outright, so
//     no button on the page had a handler.
//   - the WASM URL was document-relative and the launcher pushState'd to
//     /demo/ first, so it fetched /demo/modem.wasm and 404ed - while a
//     direct link to /demo/ worked, because the route directory carries
//     <base href="/">.
//   - the launcher buttons shipped `disabled` for page.js to undo, and a
//     visitor holding a cached page.js got a permanently dead button.
//   - the receive tally read ANSWERING before anything had arrived,
//     contradicting its own caption.
//
// `http://localhost` is a secure context, so AudioWorklet, WASM
// instantiation and the whole demo genuinely run here - this is the real
// thing, not a mock.
import { test, expect } from '@playwright/test';

const PAGES = ['/', '/explained', '/research',
  '/debugging', '/downloads', '/projects', '/about'];

/** Dismisses the consent dialog if it is showing.
 *
 * It is a native `<dialog>` opened with `showModal()`, so it is in the
 * top layer and swallows every click underneath it - a test that skips
 * this fails on "dialog intercepts pointer events" rather than on
 * anything it was written to check. Declining rather than accepting so
 * the tests never turn analytics on. */
async function dismissConsent(page) {
  const decline = page.locator('[data-consent-decline]');
  if (await decline.isVisible().catch(() => false)) await decline.click();
}

/** Fails the test on any console error or unhandled rejection.
 *
 * Attached before navigation on purpose: the defects above surfaced as a
 * single console error during module evaluation and nothing else, so a
 * check that runs after load would have missed the moment entirely. */
function failOnPageErrors(page, errors) {
  page.on('console', (msg) => {
    if (msg.type() === 'error') errors.push(`console: ${msg.text()}`);
  });
  page.on('pageerror', (err) => errors.push(`pageerror: ${err.message}`));
}

/** Whether this browser can actually start an AudioWorklet.
 *
 * `audioWorklet.addModule` needs the audio rendering thread, and in a
 * container with no output device that thread never starts - the promise
 * simply never settles and never fetches the module. Measured directly:
 * the AudioContext reports `state: "running"` and `audioWorklet` is
 * present, and the request for the worklet file is still never made. So
 * the usual capability checks all say yes and the demo still cannot run.
 *
 * The two tests below need the demo to genuinely run, so they need this.
 * They skip rather than fail where it is unavailable, and say why: a
 * permanent red gets ignored, and ignoring it would waste the fourteen
 * tests around it that do not need audio at all.
 *
 * Probed against the site's own worklet, on the site's own origin, so
 * the CSP applies exactly as it does in the real thing. */
async function audioWorkletStarts(page) {
  await page.goto('/');
  return page.evaluate(async () => {
    try {
      const Ctor = window.AudioContext || window.webkitAudioContext;
      const ctx = new Ctor();
      const settled = await Promise.race([
        ctx.audioWorklet.addModule('/wired-worklet.js').then(() => true),
        new Promise((resolve) => setTimeout(() => resolve(false), 5000)),
      ]);
      await ctx.close().catch(() => {});
      return settled;
    } catch {
      return false;
    }
  });
}

/** Makes every microphone request reject, the way a refusal does.
 *
 * Runs inside the page via `addInitScript`, so it must be
 * self-contained.
 *
 * Patches `MediaDevices.prototype`, not `navigator.mediaDevices`.
 * Assigning to the instance is the obvious way and it is not reliable:
 * it passed locally and silently did nothing on CI, where three tests
 * then waited out their whole timeout for an error caption that was
 * never going to appear, because the microphone had simply been granted.
 * `--use-fake-device-for-media-stream` means a granted request succeeds,
 * so a failed injection does not look like a failed injection - it looks
 * like the product not reporting an error.
 *
 * The prototype exists from the moment the document does, whatever
 * `navigator.mediaDevices` is doing, and it is what the call resolves
 * through either way. The instance is patched too, for any browser
 * carrying an own property that would shadow the prototype. */
function denyTheMicrophone() {
  const reject = () => {
    const e = new Error('Permission denied');
    e.name = 'NotAllowedError';
    return Promise.reject(e);
  };
  if (typeof MediaDevices !== 'undefined' && MediaDevices.prototype) {
    MediaDevices.prototype.getUserMedia = reject;
  }
  if (navigator.mediaDevices) navigator.mediaDevices.getUserMedia = reject;
  // The long-deprecated aliases, in case anything falls back to one.
  navigator.getUserMedia = reject;
}

/** Fails loudly if `denyTheMicrophone` did not take.
 *
 * This exists because the opposite happened. When the injection silently
 * missed, the microphone was granted by
 * `--use-fake-device-for-media-stream`, the endpoint started perfectly,
 * and the tests waiting for an error caption waited out their whole
 * timeout. The symptom was "the product does not report failures" and
 * the cause was "there was no failure to report", which is a long way to
 * travel in the wrong direction.
 *
 * A test that depends on injected failure has to check the injection. */
async function assertMicrophoneDenied(page) {
  const outcome = await page.evaluate(() =>
    navigator.mediaDevices
      .getUserMedia({ audio: true })
      .then(() => 'granted', (e) => e.name));
  expect(
    outcome,
    'the microphone denial did not take effect, so this test would be measuring a call that worked',
  ).toBe('NotAllowedError');
}

test.describe('every page', () => {
  for (const path of PAGES) {
    test(`${path} loads with no console errors`, async ({ page }) => {
      const errors = [];
      failOnPageErrors(page, errors);
      const response = await page.goto(path);
      expect(response.status()).toBe(200);
      await expect(page.locator('h1')).toHaveCount(1);
      await page.waitForLoadState('networkidle');
      expect(errors).toEqual([]);
    });
  }
});

test.describe('the launcher', () => {
  // The regression of 10 Sep 2026, asserted twice over: in the markup as
  // served, and in the live DOM. The markup assertion is the one that
  // matters - a button that needs JavaScript to arrive before it can be
  // pressed is dead every time that JavaScript is stale or blocked, and
  // that is exactly how it reached Dan.
  test('buttons are enabled in the served HTML, not enabled by script', async ({ page, request }) => {
    const html = await (await request.get('/')).text();
    const launcher = html.slice(html.indexOf('id="launcher"'), html.indexOf('id="launcher"') + 600);
    expect(launcher).toContain('id="launch-demo-btn"');
    expect(launcher).not.toContain('disabled');

    await page.goto('/');
    for (const id of ['#launch-demo-btn', '#launch-originate-btn', '#launch-receive-btn']) {
      await expect(page.locator(id)).toBeEnabled();
    }
  });

  // page.js restores button labels (after "Starting...", and on the way
  // back to the landing page), and it used to do that by assigning to
  // `button.textContent` - which replaces every child, so it deleted the
  // icon inside the button along with the old text. `showLanding` runs on
  // every load of the landing page, so the three launcher icons were
  // stripped before the first paint, every visit. The file had ten icons
  // and the DOM had seven.
  //
  // Counting the served markup against the live DOM is what caught it,
  // so that is what this asserts.
  test('the icons in the markup survive into the DOM', async ({ page, request }) => {
    const html = await (await request.get('/')).text();
    const inMarkup = (html.match(/class="dial-button__icon"/g) || []).length;
    expect(inMarkup).toBeGreaterThan(0);

    await page.goto('/');
    await expect(page.locator('.dial-button__icon')).toHaveCount(inMarkup);

    // And specifically the three that were being stripped.
    for (const id of ['#launch-demo-btn', '#launch-originate-btn', '#launch-receive-btn']) {
      await expect(page.locator(`${id} .dial-button__icon`)).toHaveCount(1);
    }
  });

  test('Demo fetches the WASM from the site root, not the route directory', async ({ page }) => {
    const errors = [];
    failOnPageErrors(page, errors);
    await page.goto('/');
    await dismissConsent(page);

    const wasm = page.waitForRequest((r) => r.url().endsWith('.wasm'), { timeout: 30000 });
    await page.locator('#launch-demo-btn').click();
    const request = await wasm;

    // The whole bug: pushState('/demo/') runs before init, so a
    // document-relative 'modem.wasm' resolves to /demo/modem.wasm.
    expect(new URL(request.url()).pathname).toBe('/modem.wasm');
    const response = await request.response();
    expect(response.status()).toBe(200);
    expect(errors).toEqual([]);
  });

  test('Demo brings both ends up to CONNECTED', async ({ page }) => {
    test.skip(!(await audioWorkletStarts(page)), 'no audio output device - AudioWorklet cannot start here');
    await page.goto('/');
    await dismissConsent(page);
    await page.locator('#launch-demo-btn').click();

    // The panel appears, then the real overture plays and both Sessions
    // connect. The overture alone is ~11s, so this waits generously.
    await expect(page.locator('#wired-panel')).toBeVisible({ timeout: 30000 });
    await expect(page.locator('#wired-status-a')).toHaveText('CONNECTED', { timeout: 60000 });
    await expect(page.locator('#wired-status-b')).toHaveText('CONNECTED', { timeout: 60000 });
  });
});

test.describe('the routes', () => {
  // A direct link must never start audio on its own - it must offer a
  // Start button. This is also what caught the page being inert: when
  // page.js died, this button never appeared.
  for (const [route, label] of [['/demo/', 'Start demo'], ['/originate/', 'Start originating modem'], ['/receive/', 'Start receiving modem']]) {
    test(`${route} offers its own Start button and plays nothing first`, async ({ page }) => {
      const errors = [];
      failOnPageErrors(page, errors);
      await page.goto(route);
      await expect(page.locator('#route-start-btn')).toBeVisible();
      await expect(page.locator('#route-start-btn')).toHaveText(label);
      await expect(page.locator('#wired-panel')).toBeHidden();
      await expect(page.locator('#endpoint-panel')).toBeHidden();

      // The landing page's three choices must be gone, not merely
      // scrolled past. `showRouteStart` set `launcher.hidden = true`
      // from the day the routes existed and it did nothing:
      // `.demo__controls` carries `display: flex` at the same
      // specificity as the UA sheet's `[hidden]`, so the author rule
      // won. A QR code scanned onto /receive/ therefore landed on a page
      // still offering Demo, Originating and Receiving. Asserted on
      // `toBeHidden`, which resolves computed visibility rather than the
      // attribute - the attribute was set correctly the whole time and
      // that is exactly what made this invisible for so long.
      await expect(page.locator('#launcher')).toBeHidden();

      // And the route opens as the overlay, not as a section of the
      // homepage. A direct link is arrived at deliberately - usually a
      // scanned QR code - and the visitor came for the one thing the
      // route names.
      await expect(page.locator('#route-start-block')).toBeInViewport();
      await expect(page.locator('#route-start-block')).toHaveClass(/app-mode/);
      await expect(page.locator('#route-start-back-btn')).toBeVisible();
      expect(errors).toEqual([]);
    });
  }
});

test.describe('the receiving end', () => {
  // Dan, 10 Sep 2026: "Receive says answer before the ring has even
  // started - should it not say idle". `Session::answer()` sits in
  // ANSWERING from the moment the page opens, so the tally has to follow
  // the carrier rather than the enum, and it has to agree with the
  // caption right beneath it.
  test('reads IDLE, not ANSWERING, while nothing is on the line', async ({ page, browserName }) => {
    test.skip(browserName !== 'chromium', 'needs a fake microphone device');
    test.skip(!(await audioWorkletStarts(page)), 'no audio output device - AudioWorklet cannot start here');
    await page.goto('/receive/');
    await dismissConsent(page);
    await page.locator('#route-start-btn').click();
    await expect(page.locator('#endpoint-status')).toHaveText('IDLE', { timeout: 30000 });
    await expect(page.locator('#endpoint-caption')).toHaveText('Listening for a call');
  });
});

test.describe('hidden means hidden', () => {
  // Three separate bugs, one cause: an author rule setting `display` on
  // a class ties the UA sheet's `[hidden]` at (0,1,0) and wins, so the
  // attribute did nothing. `.demo__controls` and `.dial-button` both do
  // it. Asserted through computed visibility, never the attribute - the
  // attribute was set correctly the entire time these were broken,
  // which is exactly why nobody found them for so long.
  test('an element marked hidden is not on screen, whatever its class sets', async ({ page }) => {
    await page.goto('/');
    // .dial-button sets display: inline-flex; this button ships with the
    // `hidden` attribute in the markup and had never once been honoured,
    // so "Dial now" was on the receiving modem, which never dials.
    await expect(page.locator('#endpoint-dial')).toBeHidden();
    // .demo__controls sets display: flex.
    await expect(page.locator('#route-start-block')).toBeHidden();
  });
});

test.describe('when starting the modem fails', () => {
  // The catch in startEndpointRoute used to write its diagnostic and
  // then call exitToLanding(), which tears the panel down, resets every
  // caption on it - including the one just written - and pushes the URL
  // back to /. So any failure presented as the panel flashing up and
  // vanishing with nothing said anywhere, which on a phone is
  // indistinguishable from the receiving modem never opening at all
  // (Dan, 10 Sep 2026).
  //
  // Deliberately not asserting *which* failure. A denied microphone is
  // the likeliest one on a real phone, but this runner has no audio
  // output device so it fails earlier, at the worklet. The regression
  // being guarded is the panel disappearing, and that is the same
  // whatever threw - so the test forces a failure it can rely on and
  // then checks the panel is still there, still saying something, still
  // on its own URL.
  test('the panel stays up, says why, and does not bounce home', async ({ page }) => {
    // The function itself, not a call to it. `addInitScript` serialises
    // what it is given and runs it in the page, so a closure calling a
    // helper defined out here throws ReferenceError in the browser -
    // silently, patching nothing. Caught immediately by
    // `assertMicrophoneDenied`, which is what that guard is for.
    await page.addInitScript(denyTheMicrophone);
    await page.goto('/receive/');
    await assertMicrophoneDenied(page);
    await dismissConsent(page);
    await page.locator('#route-start-btn').click();

    const caption = page.locator('#endpoint-caption');
    // 45s, derived rather than picked. This caption can arrive by two
    // routes: getUserMedia rejecting at once, or - when the worklet is
    // slow - modem.js's own WORKLET_READY_TIMEOUT_MS of 10s plus
    // RESUME_TIMEOUT_MS of 4s plus a WASM fetch, before the catch that
    // writes it is even reached. 20s left almost no headroom over that.
    //
    // Raising it did not fix the CI failure and was never going to: the
    // injection was not taking effect there, so the caption was not
    // late, it was never coming. See `denyTheMicrophone`. The larger
    // allowance is kept because the arithmetic above is still right.
    await expect(caption).not.toBeEmpty({ timeout: 45000 });
    await expect(caption).toHaveClass(/diagnostic--warning/);
    // Still the receiving modem, still on its own URL - not bounced home.
    await expect(page.locator('#endpoint-panel')).toBeVisible();
    await expect(page.locator('#endpoint-back-btn')).toBeVisible();
    await expect(page.locator('#endpoint-status')).toHaveText('IDLE');
    expect(new URL(page.url()).pathname).toBe('/receive/');
  });
});

test.describe('leaving a live panel', () => {
  // Stop was wired straight to the teardown, which hides the panel but
  // never left app mode or put the launcher back - so the page went
  // blank, with `body.app-mode`'s `overflow: hidden` still locking the
  // scroll, until a reload (Dan, 10 Sep 2026: "when you exit demo ...
  // the button have dispeared until page refresh"). Latent from the day
  // Stop was added; it only surfaced once `[hidden]` began to be
  // honoured, because until then hiding the launcher did nothing.
  for (const [label, selector] of [['Back', '#route-start-back-btn']]) {
    test(`${label} restores the landing page`, async ({ page }) => {
      await page.goto('/receive/');
      await dismissConsent(page);
      await expect(page.locator('#launcher')).toBeHidden();

      await page.locator(selector).click();

      await expect(page.locator('#launcher')).toBeVisible();
      await expect(page.locator('#launch-demo-btn')).toBeEnabled();
      await expect(page.locator('body')).not.toHaveClass(/app-mode/);
      expect(new URL(page.url()).pathname).toBe('/');
    });
  }

  // Stop is the control that was actually broken, so it is the one that
  // has to be exercised - the Back case above passed throughout, because
  // Back always went through exitToLanding.
  //
  // Reached here from a failed start: this runner has no audio output
  // device, so a working demo is not available to press Stop in, but the
  // panel and its buttons are identical either way and the wiring under
  // test is the click handler, not the audio.
  for (const [label, selector] of [['Stop', '#endpoint-stop'], ['Back', '#endpoint-back-btn']]) {
    test(`${label} on a live panel restores the landing page`, async ({ page }) => {
      await page.addInitScript(denyTheMicrophone);
      await page.goto('/receive/');
      await assertMicrophoneDenied(page);
      await dismissConsent(page);
      await page.locator('#route-start-btn').click();
      await expect(page.locator('#endpoint-caption')).not.toBeEmpty({ timeout: 45000 });

      await page.locator(selector).click();

      await expect(page.locator('#launcher')).toBeVisible();
      await expect(page.locator('#launch-demo-btn')).toBeEnabled();
      await expect(page.locator('#endpoint-panel')).toBeHidden();
      // body.app-mode sets overflow: hidden - a stranded one locks the
      // page's scroll with nothing on screen to explain why.
      await expect(page.locator('body')).not.toHaveClass(/app-mode/);
    });
  }
});

test.describe('the shared chrome', () => {
  test('the nav reaches every page and marks the current one', async ({ page }) => {
    for (const path of PAGES) {
      await page.goto(path);
      await expect(page.locator('.site-nav__link')).toHaveCount(7);
      await expect(page.locator('.site-nav__link[aria-current="page"]')).toHaveCount(1);
    }
  });

  // Removed 10 Sep 2026 in favour of /projects, which is in the nav. If
  // it ever comes back to the footer it is duplication, not a feature -
  // the same mirror-drift rule the content plan keeps for the main site.
  test('the footer does not repeat the sibling links /projects carries', async ({ page }) => {
    await page.goto('/');
    await expect(page.locator('footer')).not.toContainText('Also from DBHQ');
    await page.goto('/projects');
    await expect(page.locator('h1')).toHaveText('Also from DBHQ');
  });
});
