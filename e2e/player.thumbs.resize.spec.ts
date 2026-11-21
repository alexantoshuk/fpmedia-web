/**
 * Hover thumb decode box changes mid-session (`set_thumb_max_size`).
 * Debug panel → "Thumb size" select (80 / 160 / 320). After each change the hover
 * canvas must repaint at the new box (16:9 clip → canvas width == box).
 * Clip: AVC 1080p GOP48 (testvideo), single `<Video>`.
 * Usage: npx playwright test e2e/player.thumbs.resize.spec.ts
 */
import { expect, test, type Locator, type Page } from "@playwright/test";
import { DemoPlayer } from "./helpers/player";
import { TestvideoClip, testvideoUrl } from "./helpers/testvideo";

const CLIP_URL = testvideoUrl(TestvideoClip.gop48);

test.describe.configure({ timeout: 120_000 });

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

/** Keep hovering (jitter re-aims) until the canvas is painted at `box` wide. */
async function expectHoverAtBox(
  page: Page,
  player: DemoPlayer,
  canvas: Locator,
  box: number,
) {
  await player.scrubber.scrollIntoViewIfNeeded();
  const rect = await player.scrubber.boundingBox();
  if (!rect) throw new Error("no scrubber");
  const y = rect.y + rect.height / 2;
  const base = rect.x + rect.width * 0.4;
  let last: CanvasShot = { width: 0, height: 0, painted: false };
  const deadline = Date.now() + 40_000;
  for (let i = 0; Date.now() < deadline; i++) {
    await page.mouse.move(base + (i % 2 ? 6 : 0), y);
    await page.waitForTimeout(400);
    last = await shot(canvas);
    if (last.painted && last.width === box) return last;
  }
  throw new Error(`hover canvas never painted at ${box}: ${JSON.stringify(last)}`);
}

test("hover thumb repaints at the new max size (80 ↔ 160 ↔ 320)", async ({
  page,
}) => {
  const player = new DemoPlayer(page);
  await player.goto();
  await player.openUrl(CLIP_URL, 120_000);
  await expect(player.error).toHaveText("");

  // Debug panel holds the A/B knobs.
  await page.getByTestId("btn-debug").click();
  const select = page.getByTestId("sel-thumb-size");
  await expect(select).toBeVisible();
  await expect(select).toBeEnabled();
  await expect(select).toHaveValue("160");

  const canvas = page.getByTestId("scrubber-thumb").locator("canvas");

  const at160 = await expectHoverAtBox(page, player, canvas, 160);
  expect(at160.height).toBeLessThan(at160.width); // 16:9 aspect-fit

  for (const box of [320, 80, 160]) {
    await select.selectOption(String(box));
    await expect(select).toHaveValue(String(box));
    const s = await expectHoverAtBox(page, player, canvas, box);
    expect(s.height).toBeLessThan(s.width);
    await expect(player.error).toHaveText("");
  }
});
