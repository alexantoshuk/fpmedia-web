/**
 * Sticky UI error must clear after a successful seek/scrub settle.
 *
 * Regression: host only `error.set(Some)` on fail — a prior banner stayed
 * visible after Ok Exact. Seed via empty "Load Remote URL" (`empty URL` keeps
 * the open player); seek Ok must clear the banner (`host_ops` clear on Ok).
 */
import { expect, test } from "@playwright/test";
import { DemoPlayer, FIXTURES } from "./helpers/player";

test.describe("sticky error clear after Ok settle", () => {
  test("sticky banner then seek clears error", async ({ page }) => {
    test.setTimeout(90_000);

    const player = new DemoPlayer(page);
    await player.goto();
    await player.openFixture(FIXTURES.shortGop, 60_000);
    await expect(player.error).toHaveText("");

    await player.openEmptyUrl();
    await expect(player.error).toHaveText("empty URL");

    const last = (await player.readStatus()).lastFrame ?? 0;
    const target = Math.max(1, Math.floor(last * 0.25));
    await player.seekToFrame(target, 30_000);
    await expect(player.error).toHaveText("");
  });
});
