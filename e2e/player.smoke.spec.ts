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

    // Scrub mid-clip (not near EOF — short fixtures can End before Pause clicks).
    await player.scrubToFraction(0.4);
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
    await expect(mute).toHaveClass(/is-on/);
    // Leptos `prop:aria-label` sets the IDL/reflected name; `toHaveAttribute`
    // often sees null. Prefer accessible name (+ title mirror).
    await expect(mute).toHaveAccessibleName("Unmute");
    await expect(mute).toHaveAttribute("title", "Unmute");

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

  test("EOF then scrub then Play starts (no silent no-op)", async ({ page }) => {
    const player = new DemoPlayer(page);
    await player.goto();
    await player.openFixture(FIXTURES.shortGop);

    const last = (await player.readStatus()).lastFrame;
    expect(last).toBeGreaterThan(2);
    // Land on the last frame so Play hits EOF and leaves the media clock there.
    await player.seekToFrame(last!);
    await player.play.click();
    await expect
      .poll(async () => (await player.readStatus()).playing)
      .toBe(true);
    await expect
      .poll(async () => (await player.readStatus()).playing, {
        timeout: 20_000,
        message: "expected EOF to clear playing after last frame",
      })
      .toBe(false);

    await player.scrubToFraction(0.1);
    await expect
      .poll(async () => (await player.readStatus()).playhead ?? last!)
      .toBeLessThan(last! - 1);

    const before = (await player.readStatus()).playhead ?? 0;
    await player.play.click();
    await expect
      .poll(async () => (await player.readStatus()).playing, {
        timeout: 15_000,
        message: "Play after EOF→scrub stayed idle (PlayPrime killed?)",
      })
      .toBe(true);
    await expect
      .poll(async () => (await player.readStatus()).playhead ?? 0)
      .toBeGreaterThan(before);
    expect(await player.error.innerText()).toBe("");
  });

  test("Play then multi-tap scrubber resumes", async ({ page }) => {
    const player = new DemoPlayer(page);
    await player.goto();
    await player.openFixture(FIXTURES.shortGop);

    await player.play.click();
    await expect
      .poll(async () => (await player.readStatus()).playing)
      .toBe(true);
    await expect
      .poll(async () => (await player.readStatus()).playhead ?? 0)
      .toBeGreaterThan(0);

    const box = await player.scrubber.boundingBox();
    if (!box) throw new Error("scrubber has no bounding box");
    const y = box.y + box.height / 2;
    // Burst of taps (bug needs frequent clicks) — drag between settle/play
    // must not drop PlayPrime / clear resume latch.
    const fractions = [0.3, 0.5, 0.4, 0.6, 0.35, 0.55, 0.45];
    for (const fraction of fractions) {
      const x = box.x + box.width * fraction;
      await page.mouse.move(x, y);
      await page.mouse.down();
      await page.mouse.up();
    }

    await expect
      .poll(async () => (await player.readStatus()).playing, {
        timeout: 15_000,
        message: "multi-tap scrub while Play left player paused",
      })
      .toBe(true);
    expect(await player.error.innerText()).toBe("");
  });

  test("Pause→Play and scrub→Play resume quickly", async ({ page }) => {
    const player = new DemoPlayer(page);
    await player.goto();
    await player.openFixture(FIXTURES.withAudio);

    // Warm play then Pause.
    await player.play.click();
    await expect
      .poll(async () => (await player.readStatus()).playing)
      .toBe(true);
    await expect
      .poll(async () => (await player.readStatus()).playhead ?? 0)
      .toBeGreaterThan(0);
    await player.pause.click();
    await expect
      .poll(async () => (await player.readStatus()).playing)
      .toBe(false);

    const pausePh = (await player.readStatus()).playhead ?? 0;
    const pausePlayStart = Date.now();
    await player.play.click();
    await expect
      .poll(async () => {
        const s = await player.readStatus();
        return s.playing && (s.playhead ?? 0) > pausePh;
      }, {
        timeout: 5_000,
        message: "Pause→Play did not advance",
      })
      .toBe(true);
    const pausePlayMs = Date.now() - pausePlayStart;
    expect(pausePlayMs, `Pause→Play ${pausePlayMs}ms`).toBeLessThan(800);

    await player.pause.click();
    await expect
      .poll(async () => (await player.readStatus()).playing)
      .toBe(false);

    await player.scrubToFraction(0.45);
    await expect
      .poll(async () => {
        const s = await player.readStatus();
        return (
          s.playhead != null &&
          s.engine != null &&
          s.playhead === s.engine
        );
      }, { timeout: 10_000 })
      .toBe(true);

    const scrubPh = (await player.readStatus()).playhead ?? 0;
    const scrubPlayStart = Date.now();
    await player.play.click();
    await expect
      .poll(async () => {
        const s = await player.readStatus();
        return s.playing && (s.playhead ?? 0) > scrubPh;
      }, {
        timeout: 5_000,
        message: "scrub→Play did not advance",
      })
      .toBe(true);
    const scrubPlayMs = Date.now() - scrubPlayStart;
    expect(scrubPlayMs, `scrub→Play ${scrubPlayMs}ms`).toBeLessThan(800);
    expect(await player.error.innerText()).toBe("");
  });

  test("Pause after play densifies dense cache", async ({ page }) => {
    const player = new DemoPlayer(page);
    await player.goto();
    await player.openFixture(FIXTURES.withBFrames);

    const opened = await player.readStatus();
    expect(opened.ready).toBe(true);
    expect(opened.lastFrame).toBeGreaterThan(20);

    await player.seekToFrame(8);
    await player.play.click();
    await expect
      .poll(async () => (await player.readStatus()).playing)
      .toBe(true);
    await expect
      .poll(async () => (await player.readStatus()).playhead ?? 0)
      .toBeGreaterThan(10);

    await player.pause.click();
    await expect
      .poll(async () => (await player.readStatus()).playing)
      .toBe(false);

    // Pause must densify (or keep growing) the Exact trail.
    await expect
      .poll(async () => player.denseCacheFrames(), {
        timeout: 20_000,
        message: "dense cache did not grow after Pause (densify dead?)",
      })
      .toBeGreaterThanOrEqual(8);
    expect(await player.error.innerText()).toBe("");
  });

  test("Play preserves densify trail (no trim wipe)", async ({ page }) => {
    const player = new DemoPlayer(page);
    await player.goto();
    await player.openFixture(FIXTURES.withBFrames);

    await player.seekToFrame(5);
    // Wait for post-seek densify to warm Exact marks.
    await expect
      .poll(async () => player.denseCacheFrames(), { timeout: 20_000 })
      .toBeGreaterThanOrEqual(8);
    const warmed = await player.denseCacheFrames();

    await player.play.click();
    await expect
      .poll(async () => (await player.readStatus()).playing)
      .toBe(true);
    // Advance only a little — ahead densify must still be resident.
    await expect
      .poll(async () => (await player.readStatus()).playhead ?? 0)
      .toBeGreaterThan(6);
    await expect
      .poll(async () => (await player.readStatus()).playhead ?? 0)
      .toBeLessThan(12);

    const duringPlay = await player.denseCacheFrames();
    expect(
      duringPlay,
      `Play wiped ahead densify (${duringPlay} vs warmed ${warmed})`,
    ).toBeGreaterThanOrEqual(Math.min(warmed - 2, 6));
    expect(await player.error.innerText()).toBe("");
  });

  test("Play grows Exact trail along playhead (presented ingest)", async ({
    page,
  }) => {
    const player = new DemoPlayer(page);
    await player.goto();
    await player.openFixture(FIXTURES.withBFrames);

    // Cold start — no Pause densify; Exact should grow from presented frames only.
    // Cost gates (DPR stride / spare-time / CSS encode) may sparse the trail.
    await player.seekToFrame(0);
    await player.play.click();
    await expect
      .poll(async () => (await player.readStatus()).playing)
      .toBe(true);
    await expect
      .poll(async () => (await player.readStatus()).playhead ?? 0, {
        timeout: 15_000,
      })
      .toBeGreaterThanOrEqual(16);

    await expect
      .poll(async () => player.denseCacheFrames(), {
        timeout: 15_000,
        message: "Exact trail did not grow from Play presented ingest",
      })
      .toBeGreaterThanOrEqual(4);
    expect(await player.error.innerText()).toBe("");
  });
});
