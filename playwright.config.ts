import { defineConfig, devices } from "@playwright/test";

const baseURL = process.env.WEBMEDIA_BASE_URL ?? "http://127.0.0.1:3000";

// Bundled Chromium download is often blocked; system Chrome has WebCodecs.
// Override with PLAYWRIGHT_CHROME_CHANNEL= (empty) after `npm run e2e:install`.
const chromeChannel =
  process.env.PLAYWRIGHT_CHROME_CHANNEL !== undefined
    ? process.env.PLAYWRIGHT_CHROME_CHANNEL || undefined
    : process.platform === "win32"
      ? "chrome"
      : undefined;

/**
 * Browser e2e for the demo player.
 *
 * Expects `trunk serve` in `demo-player/` on :3000.
 * URL probes need `npm run media:fetch` → `demo-player/public/media/`.
 * Set WEBMEDIA_BASE_URL to override.
 */
export default defineConfig({
  testDir: "./e2e",
  fullyParallel: false,
  forbidOnly: !!process.env.CI,
  retries: process.env.CI ? 1 : 0,
  workers: 1,
  timeout: 60_000,
  expect: { timeout: 15_000 },
  reporter: [["list"], ["html", { open: "never" }]],
  use: {
    baseURL,
    trace: "on-first-retry",
    screenshot: "only-on-failure",
    // video needs Playwright's ffmpeg download; screenshots are enough locally
    video: "off",
  },
  projects: [
    {
      name: "chromium",
      use: {
        ...devices["Desktop Chrome"],
        ...(chromeChannel ? { channel: chromeChannel } : {}),
      },
    },
  ],
});
