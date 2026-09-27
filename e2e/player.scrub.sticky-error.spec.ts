/**
 * Sticky UI error must clear after a successful scrub settle.
 *
 * Regression: host only `error.set(Some)` on fail — a prior banner stayed
 * visible after Ok Exact settle. Seed via empty Open URL (`empty URL` keeps
 * the open player); scrub settle Ok must clear the banner.
 */
import { expect, test } from "@playwright/test";
import { DemoPlayer, FIXTURES } from "./helpers/player";

test.describe("sticky error clear after Ok settle", () => {
  test("sticky banner then scrub settle clears error", async ({ page }) => {
    test.setTimeout(90_000);

    const player = new DemoPlayer(page);
    await player.goto();
    await player.openFixture(FIXTURES.shortGop, 60_000);
    await expect(player.error).toHaveText("");

    // Empty URL sets error without dropping the open player.
    await page.getByTestId("url-input").fill("");
    await page.getByTestId("btn-open-url").click();
    await expect(player.error).toHaveText("empty URL");

    await player.scrubToFraction(0.25);
    const t0 = Date.now();
    while (Date.now() - t0 < 15_000) {
      const s = await player.readStatus();
      if (
        s.playhead != null &&
        s.engine != null &&
        s.playhead === s.engine
      ) {
        break;
      }
      await page.waitForTimeout(50);
    }

    await expect(player.error).toHaveText("");
  });
});
