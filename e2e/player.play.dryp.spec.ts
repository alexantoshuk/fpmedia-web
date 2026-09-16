/**
 * Chrome play stability on DRYP (local clip — AVC 1080p GOP~50 despite the name).
 *
 * Expects app on :3000. Override path with WEBMEDIA_PERF_CLIP_DRYP.
 */
import { expect, test } from "@playwright/test";
import fs from "node:fs";
import path from "node:path";
import { DemoPlayer } from "./helpers/player";

const downloads = process.env.USERPROFILE
  ? path.join(process.env.USERPROFILE, "Downloads")
  : "";

export const DRYP_CLIP =
  process.env.WEBMEDIA_PERF_CLIP_DRYP ??
  path.join(downloads, "DRYP_15_4K_H265_Final.mp4");

test.describe("DRYP play (Chrome)", () => {
  test.beforeEach(() => {
    test.skip(
      !fs.existsSync(DRYP_CLIP),
      `missing clip at ${DRYP_CLIP} (set WEBMEDIA_PERF_CLIP_DRYP)`,
    );
  });

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
    await player.openFixture(DRYP_CLIP);
    // Progressive open + AVC verify on ~7MB can exceed the default ready wait.
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
      // Dense may keep a pre-play densify trail; still must not stall / flush.

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
        await test.info().attach("dryp-stall", {
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
    await test.info().attach("dryp-final", {
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
