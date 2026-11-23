/**
 * Contract scenarios from `.ai/scenarios.md` that were still open:
 * play-at-last, playlist-eof, step picture, goto outside LoopZone,
 * Play chrome through scrub, playlist `[i]` readout, hover on/off while scrubbing.
 *
 * Picture checks use fixture burn-in (`e2e/helpers/picture.ts`).
 */
import { expect, test } from "@playwright/test";
import { DemoPlayer, FIXTURES } from "./helpers/player";
import { bestFrame, loadFrameSigs, readStageSig, type FrameSig } from "./helpers/picture";

const SHORT = loadFrameSigs(FIXTURES.shortGop);
const BF = loadFrameSigs(FIXTURES.withBFrames);

async function openPlaylist(player: DemoPlayer) {
  await player.fileInput.setInputFiles([
    FIXTURES.shortGop,
    FIXTURES.withBFrames,
    FIXTURES.withAudio,
  ]);
  await player.waitReady(90_000);
}

async function localFrame(player: DemoPlayer): Promise<number | null> {
  return (await player.readStatus()).playhead;
}

async function pictureIs(page: Page, refs: FrameSig[], frame: number): Promise<boolean> {
  const hit = bestFrame(await readStageSig(page), refs);
  return hit.frame === frame;
}

async function gotoText(player: DemoPlayer, text: string) {
  await player.page.getByTestId("position-display").click();
  await player.page.getByTestId("position-goto").click();
  await player.frameInput.fill(text);
  await player.frameInput.blur();
}

function position(player: DemoPlayer) {
  return player.page.getByTestId("playlist-position");
}

test.describe("scenario contracts", () => {
  test.describe.configure({ timeout: 120_000 });

  test("play-at-last restarts at frame 0", async ({ page }) => {
    const player = new DemoPlayer(page);
    await player.goto();
    await player.openFixture(FIXTURES.shortGop);
    await player.seekToFrame(47);
    expect(await pictureIs(page, SHORT, 47), "last frame picture").toBe(true);

    await player.play.click();
    let restarted = false;
    await expect
      .poll(
        async () => {
          const st = await player.readStatus();
          if (st.playhead == null || !st.playing || st.playhead > 4) return false;
          restarted = await pictureIs(page, SHORT, st.playhead);
          return restarted;
        },
        { timeout: 8_000, message: "play on the last frame did not restart at 0" },
      )
      .toBe(true);
  });

  test("playlist eof shows the next clip from its start", async ({ page }) => {
    const player = new DemoPlayer(page);
    await player.goto();
    await openPlaylist(player);
    await player.seekToFrame(47);
    await player.play.click();
    await expect
      .poll(
        async () => {
          const label = await position(player).innerText();
          if (!label.includes("[2]")) return "wait";
          const st = await player.readStatus();
          if (st.playhead == null) return "wait";
          const ok = await pictureIs(page, BF, st.playhead);
          return ok ? "ok" : `${st.playhead}`;
        },
        { timeout: 12_000, message: "clip EOF did not show the next plate" },
      )
      .toBe("ok");
  });

  test("step shows the next picture and ignores LoopZone", async ({ page }) => {
    const player = new DemoPlayer(page);
    await player.goto();
    await player.openFixture(FIXTURES.shortGop);

    await player.next.click();
    await expect.poll(async () => pictureIs(page, SHORT, 1)).toBe(true);
    await player.next.click();
    await expect.poll(async () => pictureIs(page, SHORT, 2)).toBe(true);

    await player.play.click();
    await expect.poll(async () => (await player.readStatus()).playing).toBe(true);
    const before = await localFrame(player);
    await player.next.click();
    await expect
      .poll(async () => {
        const st = await player.readStatus();
        return !st.playing && st.playhead === (before ?? 0) + 1;
      })
      .toBe(true);
    expect(await pictureIs(page, SHORT, (before ?? 0) + 1)).toBe(true);

    await player.seekToFrame(8);
    await page.keyboard.press("[");
    await player.seekToFrame(12);
    await page.keyboard.press("]");
    await player.seekToFrame(12);
    await player.next.click();
    await expect
      .poll(async () => pictureIs(page, SHORT, 13), {
        message: "Next was clamped to the loop zone",
      })
      .toBe(true);
  });

  test("goto outside LoopZone shows that frame", async ({ page }) => {
    const player = new DemoPlayer(page);
    await player.goto();
    await player.openFixture(FIXTURES.shortGop);
    await player.seekToFrame(8);
    await page.keyboard.press("[");
    await player.seekToFrame(12);
    await page.keyboard.press("]");
    await gotoText(player, "40");
    await expect
      .poll(async () => {
        const st = await player.readStatus();
        return st.playhead === 40 && !st.playing && (await pictureIs(page, SHORT, 40));
      }, { timeout: 8_000, message: "goto was clamped to the loop zone" })
      .toBe(true);
  });

  test("scrub while playing keeps the Pause button", async ({ page }) => {
    const player = new DemoPlayer(page);
    await player.goto();
    await player.openFixture(FIXTURES.shortGop);
    await player.play.click();
    await expect.poll(async () => (await player.readStatus()).playing).toBe(true);

    await player.scrubber.scrollIntoViewIfNeeded();
    const box = await player.scrubber.boundingBox();
    if (!box) throw new Error("scrubber has no bounding box");
    const y = box.y + box.height / 2;
    const x = box.x + box.width * 0.4;
    await page.mouse.move(x, y);
    await page.mouse.down();
    await expect(player.play).toHaveAttribute("title", "Pause");
    await page.mouse.move(x + 36, y, { steps: 4 });
    await expect(player.play).toHaveAttribute("title", "Pause");
    await page.mouse.up();
    await expect(player.play).toHaveAttribute("title", "Pause");
    await expect
      .poll(async () => (await player.readStatus()).playing, {
        message: "release did not keep play",
      })
      .toBe(true);
  });

  test("playlist readout is clip-local and goto honors [i]", async ({ page }) => {
    const player = new DemoPlayer(page);
    await player.goto();
    await openPlaylist(player);
    await expect(position(player)).toContainText("[1]");
    await expect(position(player)).toContainText("0");
    await expect(position(player)).toContainText("47");

    await gotoText(player, "[2] 4");
    await expect(position(player)).toContainText("[2]");
    await expect
      .poll(async () => pictureIs(page, BF, 4), {
        timeout: 8_000,
        message: "[2] 4 did not show bf frame 4",
      })
      .toBe(true);

    await gotoText(player, "9");
    await expect(position(player)).toContainText("[2]");
    await expect
      .poll(async () => pictureIs(page, BF, 9), {
        timeout: 8_000,
        message: "bare goto left the current clip",
      })
      .toBe(true);
  });

  test("hover thumb shows during play and pause, hides while scrubbing", async ({
    page,
  }) => {
    const player = new DemoPlayer(page);
    await player.goto();
    await player.openFixture(FIXTURES.shortGop);
    const thumb = page.getByTestId("scrubber-thumb");

    await player.scrubber.scrollIntoViewIfNeeded();
    const box = await player.scrubber.boundingBox();
    if (!box) throw new Error("scrubber has no bounding box");
    const x = box.x + box.width * 0.4;
    const y = box.y + box.height / 2;

    await page.mouse.move(x, y);
    await expect(thumb).toHaveClass(/is-visible/, { timeout: 8_000 });

    await page.mouse.down();
    await expect(thumb).not.toHaveClass(/is-visible/);
    await page.mouse.up();

    await player.play.click();
    await expect.poll(async () => (await player.readStatus()).playing).toBe(true);
    await page.mouse.move(x + 20, y);
    await expect(thumb).toHaveClass(/is-visible/, { timeout: 8_000 });
  });
});
