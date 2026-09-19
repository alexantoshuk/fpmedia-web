/**
 * Reverse JKL shuttle on DRYP H264 (local clip).
 *
 * Expects app on :3000. Override with WEBMEDIA_PERF_CLIP_DRYP_H264.
 */
import { expect, test } from "@playwright/test";
import fs from "node:fs";
import path from "node:path";
import { DemoPlayer } from "./helpers/player";

const downloads = process.env.USERPROFILE
  ? path.join(process.env.USERPROFILE, "Downloads")
  : "";

export const DRYP_H264_CLIP =
  process.env.WEBMEDIA_PERF_CLIP_DRYP_H264 ??
  path.join(downloads, "DRYP_15_4K_H264_Final_SHOTGRID.mp4");

test.describe("DRYP reverse shuttle (Chrome)", () => {
  test.beforeEach(() => {
    test.skip(
      !fs.existsSync(DRYP_H264_CLIP),
      `missing clip at ${DRYP_H264_CLIP} (set WEBMEDIA_PERF_CLIP_DRYP_H264)`,
    );
  });

  test("J reverse from mid-clip decreases playhead steadily", async ({ page }) => {
    test.setTimeout(120_000);

    const consoleErrors: string[] = [];
    page.on("console", (msg) => {
      if (msg.type() === "error") consoleErrors.push(msg.text());
      const text = msg.text();
      if (text.includes("flush/idle") || text.includes("backpressure timed out")) {
        consoleErrors.push(text);
      }
    });
    page.on("pageerror", (err) => consoleErrors.push(String(err)));

    const player = new DemoPlayer(page);
    await player.goto();
    await player.openFixture(DRYP_H264_CLIP, 90_000);
    await expect(player.error).toHaveText("");

    const opened = await player.readStatus();
    expect(opened.ready).toBe(true);
    const last = opened.lastFrame ?? 0;
    expect(last).toBeGreaterThan(200);

    const mid = Math.floor(last * 0.4);
    await player.frameInput.fill(String(mid));
    await player.seek.click();
    await player.waitEngine(mid, 60_000);

    // J → −2× reverse shuttle
    await page.keyboard.press("j");
    await expect
      .poll(async () => (await player.readStatus()).playing, { timeout: 15_000 })
      .toBe(true);

    const startPh = (await player.readStatus()).playhead ?? mid;
    // Allow brief settle after direction flip.
    await page.waitForTimeout(500);

    let minPh = startPh;
    let lastPh = startPh;
    let movedBack = 0;
    const deadline = Date.now() + 25_000;
    while (Date.now() < deadline && movedBack < 60) {
      const snap = await player.snapshot();
      expect(snap.error.trim(), `error at ${snap.status.playhead}`).toBe("");
      const fault = consoleErrors.find(
        (e) =>
          e.includes("flush/idle") ||
          e.includes("backpressure timed out") ||
          e.includes("decode pump"),
      );
      expect(fault, fault).toBeUndefined();

      const ph = snap.status.playhead ?? lastPh;
      if (ph < lastPh) {
        movedBack += lastPh - ph;
      }
      minPh = Math.min(minPh, ph);
      lastPh = ph;
      // Must not leap to near-zero in one burst (old dump-queue bug).
      if (startPh - ph > 5 && Date.now() < deadline - 18_000) {
        expect(
          startPh - ph,
          `playhead jumped back too fast: ${startPh} → ${ph}`,
        ).toBeLessThan(80);
      }
      await page.waitForTimeout(100);
    }

    expect(
      movedBack,
      `expected reverse progress from ~${startPh}, min=${minPh}, moved=${movedBack}`,
    ).toBeGreaterThanOrEqual(40);
    expect(minPh).toBeLessThan(startPh);

    // Engine must move (not only UI playhead) — frozen canvas left eng stuck.
    const eng = (await player.readStatus()).engine ?? mid;
    expect(eng, `engine should rewind (start≈${startPh}, eng=${eng})`).toBeLessThan(
      startPh,
    );
    expect(eng).toBeGreaterThan(Math.max(0, startPh - 250));
  });
});
