/**
 * Frame.io JKL ladder on Wing It ShotGrid (local clip).
 *
 * Expects app on :3000. Override with WEBMEDIA_PERF_CLIP_WINGIT.
 */
import { expect, test } from "@playwright/test";
import fs from "node:fs";
import path from "node:path";
import { DemoPlayer } from "./helpers/player";

const downloads = process.env.USERPROFILE
  ? path.join(process.env.USERPROFILE, "Downloads")
  : "";

export const WINGIT_CLIP =
  process.env.WEBMEDIA_PERF_CLIP_WINGIT ??
  path.join(downloads, "WING_IT_-_Blender_Open_Movie_SHOTGRID.mp4");

test.describe("Wing It JKL (Chrome)", () => {
  test.beforeEach(() => {
    test.skip(
      !fs.existsSync(WINGIT_CLIP),
      `missing clip at ${WINGIT_CLIP} (set WEBMEDIA_PERF_CLIP_WINGIT)`,
    );
  });

  test("L walks 1→2→4→8 and playhead advances", async ({ page }) => {
    test.setTimeout(120_000);

    const consoleErrors: string[] = [];
    page.on("console", (msg) => {
      if (msg.type() === "error") consoleErrors.push(msg.text());
    });
    page.on("pageerror", (err) => consoleErrors.push(String(err)));

    const player = new DemoPlayer(page);
    await player.goto();
    await player.openFixture(WINGIT_CLIP, 90_000);

    const opened = await player.readStatus();
    const last = opened.lastFrame ?? 0;
    expect(last).toBeGreaterThan(200);
    const mid = Math.floor(last * 0.35);
    await player.seekToFrame(mid);

    expect(await player.readPlaybackRate()).toBeCloseTo(1, 2);

    for (const rate of [2, 4, 8]) {
      await page.keyboard.press("l");
      await player.waitPlaybackRate(rate, 3_000);
    }

    await expect
      .poll(async () => (await player.readStatus()).playing, { timeout: 10_000 })
      .toBe(true);

    const start = (await player.readStatus()).playhead ?? mid;
    await page.waitForTimeout(1_200);
    const end = (await player.readStatus()).playhead ?? start;
    expect(end, `expected forward progress from ${start}`).toBeGreaterThan(start + 5);

    const fault = consoleErrors.find(
      (e) =>
        e.includes("flush/idle") ||
        e.includes("backpressure timed out") ||
        e.includes("decode pump"),
    );
    expect(fault, fault).toBeUndefined();
  });

  test("J walks 8→4→2→1→−1 without rate stall", async ({ page }) => {
    test.setTimeout(120_000);

    const player = new DemoPlayer(page);
    await player.goto();
    await player.openFixture(WINGIT_CLIP, 90_000);

    const last = (await player.readStatus()).lastFrame ?? 0;
    const mid = Math.floor(last * 0.4);
    await player.seekToFrame(mid);

    // Climb to 8× first.
    for (const rate of [2, 4, 8]) {
      await page.keyboard.press("l");
      await player.waitPlaybackRate(rate, 3_000);
    }

    for (const rate of [4, 2, 1, -1]) {
      const t0 = Date.now();
      await page.keyboard.press("j");
      await player.waitPlaybackRate(rate, 3_000);
      expect(
        Date.now() - t0,
        `rate → ${rate} took too long (UI lag)`,
      ).toBeLessThan(2_500);
    }

    await expect
      .poll(async () => (await player.readStatus()).playing, { timeout: 10_000 })
      .toBe(true);
  });

  test("reverse −2× from mid-clip moves engine backward", async ({ page }) => {
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
    await player.openFixture(WINGIT_CLIP, 90_000);

    const last = (await player.readStatus()).lastFrame ?? 0;
    const mid = Math.floor(last * 0.45);
    await player.seekToFrame(mid);

    // 1 → −1 (direction flip + invalidate) — let reverse warm before −2.
    await page.keyboard.press("j");
    await player.waitPlaybackRate(-1, 3_000);
    await expect
      .poll(async () => (await player.readStatus()).playing, { timeout: 15_000 })
      .toBe(true);
    await page.waitForTimeout(1_500);
    await expect(player.error).toHaveText("", { timeout: 15_000 });

    await page.keyboard.press("j");
    await player.waitPlaybackRate(-2, 3_000);
    await page.waitForTimeout(500);
    await expect(player.error).toHaveText("", { timeout: 15_000 });

    const startPh = (await player.readStatus()).playhead ?? mid;
    await page.waitForTimeout(600);

    let minPh = startPh;
    let lastPh = startPh;
    let movedBack = 0;
    const deadline = Date.now() + 30_000;
    while (Date.now() < deadline && movedBack < 30) {
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
      await page.waitForTimeout(120);
    }

    expect(
      movedBack,
      `expected reverse progress from ~${startPh}, min=${minPh}, moved=${movedBack}`,
    ).toBeGreaterThanOrEqual(20);

    const eng = (await player.readStatus()).engine ?? mid;
    expect(eng, `engine should rewind (start≈${startPh}, eng=${eng})`).toBeLessThan(
      startPh,
    );
  });
});
