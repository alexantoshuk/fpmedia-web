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

  test("reverse −1× from mid-clip keeps moving (not dense slideshow)", async ({
    page,
  }) => {
    test.setTimeout(120_000);

    const consoleErrors: string[] = [];
    page.on("console", (msg) => {
      if (msg.type() === "error") consoleErrors.push(msg.text());
    });
    page.on("pageerror", (err) => consoleErrors.push(String(err)));

    const player = new DemoPlayer(page);
    await player.goto();
    await player.openFixture(WINGIT_CLIP, 90_000);

    const status0 = await player.readStatus();
    expect(status0.raw, `status=${status0.raw}`).toMatch(/GOP\s*30/i);

    const last = status0.lastFrame ?? 0;
    const mid = Math.floor(last * 0.45);
    await player.seekToFrame(mid);

    await page.keyboard.press("j");
    await player.waitPlaybackRate(-1, 3_000);
    await expect
      .poll(async () => (await player.readStatus()).playing, { timeout: 15_000 })
      .toBe(true);
    await expect(player.error).toHaveText("", { timeout: 15_000 });

    const startEng = (await player.readStatus()).engine ?? mid;
    // Must not dump to the start (old key-hop snap avalanche).
    expect(startEng).toBeGreaterThan(mid - 120);

    // First reverse GOP is a short forward decode pass (mediabunny #350) while
    // the clock holds — do not count that prime toward steady-state stall.
    let primed = startEng;
    const primeDeadline = Date.now() + 15_000;
    while (Date.now() < primeDeadline) {
      await page.waitForTimeout(100);
      const eng = (await player.readStatus()).engine ?? primed;
      expect(eng, `eng dumped toward 0 (start=${startEng})`).toBeGreaterThan(
        Math.max(0, startEng - 500),
      );
      if (eng < primed) {
        primed = eng;
        break;
      }
    }
    expect(primed, `−1× never left startEng=${startEng}`).toBeLessThan(startEng);

    const t0 = Date.now();
    let lastEng = primed;
    let moved = 0;
    let maxStallMs = 0;
    let stallStart = t0;

    while (Date.now() - t0 < 12_000 && moved < 60) {
      await page.waitForTimeout(200);
      const snap = await player.snapshot();
      expect(snap.error.trim()).toBe("");
      const eng = snap.status.engine ?? lastEng;
      // Guard dump-to-zero.
      expect(eng, `eng dumped toward 0 (start=${startEng})`).toBeGreaterThan(
        Math.max(0, startEng - 500),
      );
      if (eng < lastEng) {
        moved += lastEng - eng;
        stallStart = Date.now();
      } else {
        maxStallMs = Math.max(maxStallMs, Date.now() - stallStart);
      }
      lastEng = eng;
    }

    expect(
      moved,
      `−1× should rewind densely (startEng=${startEng}, moved=${moved})`,
    ).toBeGreaterThanOrEqual(30);
    expect(maxStallMs, `−1× stalled ${maxStallMs}ms`).toBeLessThan(3_000);

    const end = await player.readStatus();
    expect(end.playing).toBe(true);
    expect(end.engine ?? 0).toBeGreaterThan(0);

    const fault = consoleErrors.find(
      (e) => e.includes("flush/idle") || e.includes("backpressure timed out"),
    );
    expect(fault, fault).toBeUndefined();
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
