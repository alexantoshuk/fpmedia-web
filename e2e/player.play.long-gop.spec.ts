/**
 * Chrome play stability on long-GOP plate (AVC 1080p 24fps GOP48 bf0).
 * Clip: testvideo `avc_1080p_24_gop48_bf0`.
 * Expects app on :3000 + network (`/proxy`).
 */
import { expect, test } from "@playwright/test";
import { DemoPlayer } from "./helpers/player";
import { TestvideoClip, testvideoUrl } from "./helpers/testvideo";

const CLIP_URL = testvideoUrl(TestvideoClip.gop48);

test.describe("long-GOP play (Chrome)", () => {
  test("plays through without flush/idle or stall", async ({ page }) => {
    test.setTimeout(180_000);

    const consoleErrors: string[] = [];
    page.on("console", (msg) => {
      if (msg.type() === "error") {
        consoleErrors.push(msg.text());
      }
      const text = msg.text();
      if (text.includes("flush/idle") || text.includes("backpressure timed out")) {
        consoleErrors.push(text);
      }
    });
    page.on("pageerror", (err) => {
      consoleErrors.push(String(err));
    });

    const player = new DemoPlayer(page);
    await player.goto();
    await player.openUrl(CLIP_URL, 180_000);
    await expect(player.error).toHaveText("");

    const opened = await player.readStatus();
    expect(opened.ready).toBe(true);
    const last = opened.lastFrame ?? 0;
    expect(last).toBeGreaterThan(100);

    await player.play.click();
    await expect
      .poll(async () => (await player.readStatus()).playing, { timeout: 15_000 })
      .toBe(true);

    const target = Math.floor(last * 0.98);
    let peak = opened.playhead ?? 0;
    let peakCache = 0;
    let stuckSince = Date.now();
    const deadline = Date.now() + 150_000;

    while (Date.now() < deadline) {
      const snap = await player.snapshot();
      const err = snap.error.trim();
      expect(err, `player error at frame ${snap.status.playhead}: ${err}`).toBe(
        "",
      );

      const webcodecsFault = consoleErrors.find(
        (e) =>
          e.includes("flush/idle") ||
          e.includes("backpressure timed out") ||
          e.includes("decode pump"),
      );
      expect(
        webcodecsFault,
        `console webcodecs fault at frame ${snap.status.playhead}: ${webcodecsFault}`,
      ).toBeUndefined();

      const cacheFrames = await player.denseCacheFrames();
      peakCache = Math.max(peakCache, cacheFrames);

      const ph = snap.status.playhead ?? 0;

      if (ph > peak) {
        peak = ph;
        stuckSince = Date.now();
      } else if (Date.now() - stuckSince > 8_000 && snap.status.playing) {
        const attach = {
          snap,
          peak,
          peakCache,
          consoleErrors: consoleErrors.slice(-20),
        };
        await test.info().attach("long-gop-stall", {
          body: JSON.stringify(attach, null, 2),
          contentType: "application/json",
        });
        expect(
          ph,
          `playhead stuck at ${ph} (peak ${peak}) for >8s while playing`,
        ).toBeGreaterThan(peak);
      }

      if (ph >= target) {
        break;
      }
      await page.waitForTimeout(250);
    }

    const final = await player.snapshot();
    await test.info().attach("long-gop-final", {
      body: JSON.stringify(
        { final, peak, peakCache, consoleErrors: consoleErrors.slice(-30) },
        null,
        2,
      ),
      contentType: "application/json",
    });
    expect(final.error.trim()).toBe("");
    expect(final.status.playhead ?? 0).toBeGreaterThanOrEqual(target);
  });
});
