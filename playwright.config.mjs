// Playwright, for the browser smoke tests in web/tests/.
//
// Serves ./dist - the exact tree scripts/build-dist.sh assembles for
// Cloudflare Pages, route directories and all - so what the tests drive
// is what ships, not the loose files in web/.
//
// Through scripts/serve-dist.py rather than `python3 -m http.server`,
// because Pages resolves /explained to explained.html and /demo/ to
// demo/index.html. A plain static server 404s every extensionless path,
// which fails the tests on the server instead of on the site.
//
// `http://localhost` is a secure context as far as the browser is
// concerned, so AudioWorklet and WASM instantiation genuinely work and
// the demo really runs. No HTTPS, no certificate, no mocking.
import { defineConfig, devices } from '@playwright/test';

const PORT = 4173;

export default defineConfig({
  testDir: './web/tests',
  // The demo waits on a real overture (~11s) and a real connection, so
  // the per-test default has to allow for it.
  timeout: 90_000,
  expect: { timeout: 15_000 },
  // A flake here would be a bug, not noise: every one of these tests
  // pins something that shipped broken. Retrying would hide exactly the
  // intermittency they exist to catch.
  retries: 0,
  fullyParallel: false,
  workers: 1,
  reporter: process.env.CI ? [['github'], ['list']] : [['list']],
  use: {
    baseURL: `http://localhost:${PORT}`,
    trace: 'retain-on-failure',
    video: 'retain-on-failure',
  },
  projects: [
    {
      name: 'chromium',
      use: {
        ...devices['Desktop Chrome'],
        // The full Chromium build, not Playwright's headless shell. The
        // shell has no audio stack, so `audioWorklet.addModule` never
        // settles there and the demo can never start - the tests would be
        // measuring the browser rather than the site.
        channel: 'chromium',
        launchOptions: {
          args: [
            // The two-device routes ask for a microphone. Without these
            // the permission prompt blocks and the test hangs rather
            // than failing usefully.
            '--use-fake-ui-for-media-stream',
            '--use-fake-device-for-media-stream',
            // Chromium blocks audio until a gesture by default, which is
            // the behaviour we WANT to test - so this is deliberately
            // not disabled. The demo's own click is the gesture.
          ],
        },
      },
    },
    {
      // A phone viewport, because the failures Dan hit were mobile ones:
      // an early tap, a panel sized from a viewport read at the wrong
      // moment. Same tests, narrow screen.
      name: 'mobile-chrome',
      use: {
        ...devices['Pixel 7'],
        channel: 'chromium',
        launchOptions: {
          args: ['--use-fake-ui-for-media-stream', '--use-fake-device-for-media-stream'],
        },
      },
    },
  ],
  webServer: {
    command: `python3 scripts/serve-dist.py ${PORT}`,
    port: PORT,
    reuseExistingServer: !process.env.CI,
    timeout: 30_000,
  },
});
