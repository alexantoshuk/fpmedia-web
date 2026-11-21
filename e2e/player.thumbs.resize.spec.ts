/**
 * Hover thumb decode box changes mid-session (`set_thumb_max_size`).
 * Debug panel → "Thumb size" select (80 / 160 / 320). After each change the hover
 * canvas must repaint at the new box (16:9 clip → canvas width == box).
 * Clips: AVC 1080p GOP48 / GOP12 (testvideo). Single `<Video>` and a 2-clip
 * `<VideoPlaylist>` (`a;b` in the URL prompt; hover on both clips → VT cache +
 * producer path).
 * Usage: npx playwright test e2e/player.thumbs.resize.spec.ts
 */
import { expect, test, type Locator, type Page } from "@playwright/test";
import { DemoPlayer } from "./helpers/player";
import { TestvideoClip, testvideoUrl } from "./helpers/testvideo";

const CLIP_URL = testvideoUrl(TestvideoClip.gop48);
const PLAYLIST_URLS = [
  testvideoUrl(TestvideoClip.gop48),
  testvideoUrl(TestvideoClip.gop12),
].join(";");

test.describe.configure({ timeout: 180_000 });

interface CanvasShot {
  width: number;
  height: number;
  painted: boolean;
}

async function shot(canvas: Locator): Promise<CanvasShot> {
  return canvas.evaluate((el) => {
    const c = el as HTMLCanvasElement;
    const ctx = c.getContext("2d");
    let painted = false;
    if (ctx && c.width > 0 && c.height > 0) {
      const d = ctx.getImageData(0, 0, c.width, c.height).data;
      for (let i = 3; i < d.length; i += 4) {
        if (d[i] > 0) {
          painted = true;
          break;
        }
      }
    }
    return { width: c.width, height: c.height, painted };
  });
}

/** Keep hovering at `frac` (jitter re-aims) until the canvas is painted at `box` wide. */
async function expectHoverAtBox(
  page: Page,
  player: DemoPlayer,
  canvas: Locator,
  box: number,
  frac: number,
) {
  await player.scrubber.scrollIntoViewIfNeeded();
  const rect = await player.scrubber.boundingBox();
  if (!rect) throw new Error("no scrubber");
  const y = rect.y + rect.height / 2;
  const base = rect.x + rect.width * frac;
  let last: CanvasShot = { width: 0, height: 0, painted: false };
  const deadline = Date.now() + 40_000;
  for (let i = 0; Date.now() < deadline; i++) {
    await page.mouse.move(base + (i % 2 ? 6 : 0), y);
    await page.waitForTimeout(400);
    last = await shot(canvas);
    if (last.painted && last.width === box) return last;
  }
  throw new Error(
    `hover canvas never painted at ${box} (frac ${frac}): ${JSON.stringify(last)}`,
  );
}

/** 160 → 320 → 80 → 160, hovering each of `fracs` at every size. */
async function cycleSizes(page: Page, player: DemoPlayer, fracs: number[]) {
  // Debug panel holds the A/B knobs.
  await page.getByTestId("btn-debug").click();
  const select = page.getByTestId("sel-thumb-size");
  await expect(select).toBeVisible();
  await expect(select).toBeEnabled();
  await expect(select).toHaveValue("160");

  const canvas = page.getByTestId("scrubber-thumb").locator("canvas");

  for (const frac of fracs) {
    const s = await expectHoverAtBox(page, player, canvas, 160, frac);
    expect(s.height).toBeLessThan(s.width); // 16:9 aspect-fit
  }

  for (const box of [320, 80, 160]) {
    await select.selectOption(String(box));
    await expect(select).toHaveValue(String(box));
    for (const frac of fracs) {
      const s = await expectHoverAtBox(page, player, canvas, box, frac);
      expect(s.height).toBeLessThan(s.width);
      await expect(player.error).toHaveText("");
    }
  }
}

test("single: hover thumb repaints at the new max size (80 ↔ 160 ↔ 320)", async ({
  page,
}) => {
  const player = new DemoPlayer(page);
  await player.goto();
  await player.openUrl(CLIP_URL, 120_000);
  await expect(player.error).toHaveText("");
  await cycleSizes(page, player, [0.4]);
});

test("playlist: hover thumb repaints at the new max size on both clips", async ({
  page,
}) => {
  const player = new DemoPlayer(page);
  await player.goto();
  await player.openUrl(PLAYLIST_URLS, 150_000);
  await expect(player.error).toHaveText("");
  // Guard: really the playlist host (meta line: `playlist · clip i/n · …`).
  await expect(player.meta).toContainText("playlist · clip");
  // ~0.25 = clip 1 (live slot), ~0.75 = clip 2 (off-slot → VT cache / producer).
  await cycleSizes(page, player, [0.25, 0.75]);
});
