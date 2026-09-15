import { expect, test } from "@playwright/test";
import { DemoPlayer, FIXTURES } from "./helpers/player";

test.describe("demo player smoke", () => {
  test("open fixture, step, seek, scrub, play/pause", async ({ page }) => {
    const player = new DemoPlayer(page);
    await player.goto();
    await player.openFixture(FIXTURES.shortGop);

    const afterOpen = await player.readStatus();
    expect(afterOpen.ready).toBe(true);
    expect(afterOpen.playhead).toBe(0);
    expect(afterOpen.lastFrame).toBeGreaterThan(0);
    expect(await player.ariaPlayhead()).toBe(0);

    await player.next.click();
    await player.waitPlayhead(1);
    await player.waitEngine(1);

    await page.keyboard.press("ArrowRight");
    await player.waitPlayhead(2);

    await page.keyboard.press("ArrowLeft");
    await player.waitPlayhead(1);

    await player.seekToFrame(5);
    expect(await player.ariaPlayhead()).toBe(5);
    const scrubPct = await player.scrubberPercent();
    expect(scrubPct).toBeGreaterThan(0);
    expect(scrubPct).toBeLessThan(100);

    // Scrub near the end, then settle.
    await player.scrubToFraction(0.9);
    await expect
      .poll(async () => (await player.readStatus()).playhead)
      .toBeGreaterThan(5);

    const beforePlay = (await player.readStatus()).playhead ?? 0;
    await player.play.click();
    await expect
      .poll(async () => (await player.readStatus()).playing)
      .toBe(true);
    await expect
      .poll(async () => (await player.readStatus()).playhead ?? 0)
      .toBeGreaterThan(beforePlay);

    await player.pause.click();
    await expect
      .poll(async () => (await player.readStatus()).playing)
      .toBe(false);

    const snap = await player.snapshot();
    expect(snap.error).toBe("");
    expect(snap.meta.length).toBeGreaterThan(0);
    // Helpful when diagnosing hangs — printed in list reporter on failure via attach.
    await test.info().attach("player-snapshot", {
      body: JSON.stringify(snap, null, 2),
      contentType: "application/json",
    });
  });

  test("open B-frame fixture and step across frames", async ({ page }) => {
    const player = new DemoPlayer(page);
    await player.goto();
    await player.openFixture(FIXTURES.withBFrames);

    const s0 = await player.readStatus();
    expect(s0.ready).toBe(true);
    expect(s0.lastFrame).toBeGreaterThan(0);

    for (let i = 0; i < 3; i++) {
      await player.next.click();
      await player.waitPlayhead(i + 1);
      await player.waitEngine(i + 1);
    }

    expect(await player.error.innerText()).toBe("");
  });

  test("open audio fixture enables volume and mute", async ({ page }) => {
    const player = new DemoPlayer(page);
    await player.goto();
    await player.openFixture(FIXTURES.withAudio);

    const s0 = await player.readStatus();
    expect(s0.ready).toBe(true);
    await expect(player.meta).toContainText("audio");

    const mute = page.getByTestId("btn-mute");
    const volume = page.getByTestId("volume-input");
    await expect(mute).toBeEnabled();
    await expect(volume).toBeEnabled();

    await mute.click();
    await expect(mute).toHaveAttribute("aria-pressed", "true");
    await expect(mute).toHaveText("Unmute");

    await volume.fill("0.25");
    await expect(volume).toHaveValue("0.25");

    await player.play.click();
    await expect
      .poll(async () => (await player.readStatus()).playing)
      .toBe(true);
    await player.pause.click();
    await expect
      .poll(async () => (await player.readStatus()).playing)
      .toBe(false);
    expect(await player.error.innerText()).toBe("");
  });
});
