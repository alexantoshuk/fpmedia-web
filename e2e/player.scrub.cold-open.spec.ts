/**
 * Cold open + immediate scrub: must not freeze the tab while gray/mdat warm.
 *
 * Repro: open fMP4 URL, drag/tap scrubber across the timeline before densify has
 * resident bytes. Densify waits for FetchMap coverage (prefetch only) instead
 * of decode-stamping cold Ranges. Cold settle/seek soft-presents (no
 * goto_frame_with_interim) until the GOP is warm.
 *
 * Clip: testvideo `avc_1080p_24_fmp4_gop12_bf0`.
 */
import { expect, test } from "@playwright/test";
import { DemoPlayer } from "./helpers/player";
import { TestvideoClip, testvideoUrl } from "./helpers/testvideo";

const CLIP_URL = testvideoUrl(TestvideoClip.fmp4);

/** Fail if the page event loop does not answer within `ms`. */
async function assertResponsive(
  page: import("@playwright/test").Page,
  ms = 5_000,
) {
  const ok = await Promise.race([
    page.evaluate(() => 1 + 1).then((v) => v === 2),
    page.waitForTimeout(ms).then(() => false),
  ]);
  expect(ok, `page unresponsive >${ms}ms (likely densify/URL hang)`).toBe(
    true,
  );
}

test.describe("cold open immediate scrub", () => {
  test("fMP4 URL: drag scrubber while cold stays responsive", async ({
    page,
  }) => {
    test.setTimeout(240_000);

    const faults: string[] = [];
    page.on("console", (msg) => {
      const t = msg.text();
      if (
        msg.type() === "error" ||
        t.includes("flush/idle") ||
        t.includes("backpressure timed out")
      ) {
        faults.push(t);
      }
    });
    page.on("pageerror", (err) => faults.push(String(err)));

    const player = new DemoPlayer(page);
    await player.goto();
    // Ready as soon as warm moofs allow UI — gray strip / densify still cold.
    await player.openUrl(CLIP_URL, 180_000);
    await expect(player.error).toHaveText("");

    const box = await player.scrubber.boundingBox();
    if (!box) throw new Error("scrubber has no bounding box");
    const y = box.y + box.height / 2;

    await assertResponsive(page);

    await page.mouse.move(box.x + 2, y);
    await page.mouse.down();

    // Sweep the timeline quickly (soft-drag densify path).
    const steps = 40;
    for (let i = 0; i <= steps; i++) {
      const x = box.x + (box.width * i) / steps;
      await page.mouse.move(x, y);
      if (i % 5 === 0) {
        await assertResponsive(page, 8_000);
      }
      await page.waitForTimeout(30);
    }

    await page.mouse.up();
    await assertResponsive(page, 8_000);
    // Settle at ~mid — soft cold settle if still uncached; must not hang.
    await player.scrubToFraction(0.5);
    await assertResponsive(page, 15_000);

    expect(faults, faults.join("\n")).toEqual([]);
  });

  test("fMP4 URL: rapid taps on uncached zones stay responsive", async ({
    page,
  }) => {
    test.setTimeout(240_000);

    const faults: string[] = [];
    page.on("console", (msg) => {
      const t = msg.text();
      if (
        msg.type() === "error" ||
        t.includes("flush/idle") ||
        t.includes("backpressure timed out")
      ) {
        faults.push(t);
      }
    });
    page.on("pageerror", (err) => faults.push(String(err)));

    const player = new DemoPlayer(page);
    await player.goto();
    await player.openUrl(CLIP_URL, 180_000);
    await expect(player.error).toHaveText("");

    await assertResponsive(page);

    // Click-settle across cold gray — each tap used to await cold Exact cascade.
    const fractions = [0.15, 0.85, 0.35, 0.7, 0.5, 0.95, 0.2];
    for (const f of fractions) {
      await player.scrubToFraction(f);
      await assertResponsive(page, 8_000);
      await page.waitForTimeout(40);
    }

    await assertResponsive(page, 15_000);
    expect(faults, faults.join("\n")).toEqual([]);
  });

  test("fMP4 URL: after rapid cold taps Play is enabled and starts", async ({
    page,
  }) => {
    test.setTimeout(240_000);

    const faults: string[] = [];
    page.on("console", (msg) => {
      const t = msg.text();
      if (
        msg.type() === "error" ||
        t.includes("flush/idle") ||
        t.includes("backpressure timed out")
      ) {
        faults.push(t);
      }
    });
    page.on("pageerror", (err) => faults.push(String(err)));

    const player = new DemoPlayer(page);
    await player.goto();
    await player.openUrl(CLIP_URL, 180_000);
    await expect(player.error).toHaveText("");

    const fractions = [0.2, 0.8, 0.4, 0.9, 0.3];
    for (const f of fractions) {
      await player.scrubToFraction(f);
      await assertResponsive(page, 8_000);
      await page.waitForTimeout(40);
    }

    await assertResponsive(page, 15_000);

    // Soft cold settle used to leave busy/Settling — Play stayed dead.
    await expect(player.play).toBeEnabled({ timeout: 15_000 });
    await player.play.click();
    await expect
      .poll(async () => (await player.readStatus()).playing, {
        timeout: 30_000,
      })
      .toBe(true);
    await assertResponsive(page, 15_000);

    expect(faults, faults.join("\n")).toEqual([]);
  });
});
