/**
 * fMP4 gray strip past ~frame 1222: play through without decode faults.
 * Moof-/sub-pixel FetchMap holes are soft-merged in `file_fractions` (unit-tested);
 * this probe guards play/scrub through the reported seam.
 *
 * Clip: testvideo `avc_1080p_24_fmp4_gop12_bf0`.
 */
import { expect, test } from "@playwright/test";
import { DemoPlayer } from "./helpers/player";
import { TestvideoClip, testvideoUrl } from "./helpers/testvideo";

const CLIP_URL = testvideoUrl(TestvideoClip.fmp4);
const TARGET = 1222;

test.describe("fMP4 FetchMap / gray strip @ ~1222", () => {
  test("play through 1222 without decode faults", async ({ page }) => {
    test.setTimeout(300_000);

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

    const opened = await player.readStatus();
    expect(opened.lastFrame ?? 0).toBeGreaterThan(TARGET);

    await player.seekToFrame(Math.max(0, TARGET - 120));
    await player.play.click();
    await expect
      .poll(async () => (await player.readStatus()).playing, {
        timeout: 15_000,
      })
      .toBe(true);

    const deadline = Date.now() + 120_000;
    while (Date.now() < deadline) {
      const st = await player.readStatus();
      if ((st.playhead ?? 0) >= TARGET + 30) break;
      expect(st.playing, `stopped early @ ${st.playhead}`).toBe(true);
      await page.waitForTimeout(200);
    }

    const st = await player.readStatus();
    expect(st.playhead ?? 0).toBeGreaterThanOrEqual(TARGET);
    expect(faults, faults.join("\n")).toEqual([]);
  });
});
