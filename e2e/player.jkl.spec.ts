/**
 * Demo JKL forward shuttle (AVC 1080p 24fps GOP12 bf0).
 *
 * Ladder: 1 / 2 / 4 / 8. J slower, L faster, K → 1× + play/pause
 * (unlike Space, which toggles without resetting rate).
 * Expects app on :3000 + network (Trunk `/media/`).
 */
import { expect, test } from "@playwright/test";
import { DemoPlayer } from "./helpers/player";
import { TestvideoClip, testvideoUrl } from "./helpers/testvideo";

const CLIP_URL = testvideoUrl(TestvideoClip.gop12);

test.describe("JKL shuttle (Chrome)", () => {
  test("L walks 1→2→4→8 and playhead advances", async ({ page }) => {
    test.setTimeout(120_000);

    const consoleErrors: string[] = [];
    page.on("console", (msg) => {
      if (msg.type() === "error") consoleErrors.push(msg.text());
    });
    page.on("pageerror", (err) => consoleErrors.push(String(err)));

    const player = new DemoPlayer(page);
    await player.goto();
    await player.openUrl(CLIP_URL, 120_000);

    const opened = await player.readStatus();
    const last = opened.lastFrame ?? 0;
    expect(last).toBeGreaterThan(200);
    const mid = Math.floor(last * 0.35);
    await player.seekToFrame(mid);
    // Seek latches `busy` until OpQueue drains — L→play while busy stacks PlayPrime
    // and can leave transport stuck (Play disabled, rate already 8×).
    await expect(player.play).toBeEnabled({ timeout: 30_000 });

    expect(await player.readPlaybackRate()).toBeCloseTo(1, 2);

    await page.keyboard.press("l");
    await player.waitPlaybackRate(2, 3_000);
    await expect
      .poll(async () => (await player.readStatus()).playing, { timeout: 15_000 })
      .toBe(true);

    for (const rate of [4, 8]) {
      await page.keyboard.press("l");
      await player.waitPlaybackRate(rate, 3_000);
    }

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

  test("J walks 8→4→2→1 without rate stall", async ({ page }) => {
    test.setTimeout(120_000);

    const player = new DemoPlayer(page);
    await player.goto();
    await player.openUrl(CLIP_URL, 120_000);

    const last = (await player.readStatus()).lastFrame ?? 0;
    const mid = Math.floor(last * 0.4);
    await player.seekToFrame(mid);
    await expect(player.play).toBeEnabled({ timeout: 30_000 });

    await page.keyboard.press("l");
    await player.waitPlaybackRate(2, 3_000);
    await expect
      .poll(async () => (await player.readStatus()).playing, { timeout: 15_000 })
      .toBe(true);

    for (const rate of [4, 8]) {
      await page.keyboard.press("l");
      await player.waitPlaybackRate(rate, 3_000);
    }

    for (const rate of [4, 2, 1]) {
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

  test("K resets to 1× and toggles play; Space preserves rate", async ({ page }) => {
    test.setTimeout(120_000);

    const player = new DemoPlayer(page);
    await player.goto();
    await player.openUrl(CLIP_URL, 120_000);

    const last = (await player.readStatus()).lastFrame ?? 0;
    const mid = Math.floor(last * 0.3);
    await player.seekToFrame(mid);
    await expect(player.play).toBeEnabled({ timeout: 30_000 });

    await page.keyboard.press("l");
    await player.waitPlaybackRate(2, 3_000);
    await expect
      .poll(async () => (await player.readStatus()).playing, { timeout: 10_000 })
      .toBe(true);

    await page.keyboard.press(" ");
    await expect
      .poll(async () => (await player.readStatus()).playing, { timeout: 5_000 })
      .toBe(false);
    expect(await player.readPlaybackRate()).toBeCloseTo(2, 2);

    await page.keyboard.press(" ");
    await expect
      .poll(async () => (await player.readStatus()).playing, { timeout: 5_000 })
      .toBe(true);
    expect(await player.readPlaybackRate()).toBeCloseTo(2, 2);

    await page.keyboard.press("k");
    await expect
      .poll(async () => (await player.readStatus()).playing, { timeout: 5_000 })
      .toBe(false);
    expect(await player.readPlaybackRate()).toBeCloseTo(1, 2);

    await page.keyboard.press("k");
    await expect
      .poll(async () => (await player.readStatus()).playing, { timeout: 5_000 })
      .toBe(true);
    expect(await player.readPlaybackRate()).toBeCloseTo(1, 2);
  });
});
