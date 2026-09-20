/**
 * After a multi-second densify drag, settle must get exact without scrub HW
 * staying busy (densify storm yielding).
 * Clip: AVC 1080p 24fps GOP48 bf0 (testvideo).
 * Usage: npx playwright test e2e/perf.scrub.drag-settle.spec.ts
 */
import { expect, test } from "@playwright/test";
import { DemoPlayer } from "./helpers/player";
import { TestvideoClip, testvideoUrl } from "./helpers/testvideo";

const CLIP_URL = testvideoUrl(TestvideoClip.gop48);

test.describe.configure({ timeout: 180_000 });

test("long densify drag then settle yields scrub HW for exact", async ({
  page,
}) => {
  const player = new DemoPlayer(page);
  await player.goto();
  await player.openUrl(CLIP_URL, 180_000);

  const box = await player.scrubber.boundingBox();
  if (!box) throw new Error("no scrubber");
  const y = box.y + box.height / 2;
  const x = (f: number) => box.x + box.width * Math.min(1, Math.max(0, f));

  // Long drag across ~1/3 of the timeline — densify storms multiple GOPs.
  await page.mouse.move(x(0.1), y);
  await page.mouse.down();
  const steps = 48;
  for (let i = 1; i <= steps; i++) {
    await page.mouse.move(x(0.1 + (0.35 * i) / steps), y);
    if (i % 8 === 0) await page.waitForTimeout(40);
  }

  const settleStart = Date.now();
  await page.mouse.up();

  let denseWhileSettling = 0;
  let settledMs: number | null = null;
  let lastCache = "";
  while (Date.now() - settleStart < 45_000) {
    await page.waitForTimeout(100);
    const snap = await player.snapshot();
    lastCache = snap.frameCache;
    if (lastCache.includes("· dense")) denseWhileSettling += 1;
    const ph = snap.status.playhead;
    const eng = snap.status.engine;
    if (ph != null && eng != null && ph === eng) {
      settledMs = Date.now() - settleStart;
      break;
    }
  }

  const report = {
    settledMs,
    timedOut: settledMs == null,
    denseSamplesWhileSettling: denseWhileSettling,
    endCache: lastCache,
  };
  await test.info().attach("drag-settle-report", {
    body: JSON.stringify(report, null, 2),
    contentType: "application/json",
  });

  expect(settledMs, `exact not painted: ${JSON.stringify(report)}`).not.toBeNull();
  // After pointer-up, densify must yield quickly — not stay busy for the whole
  // settle cascade. Allow a few samples for abort to land.
  expect(
    denseWhileSettling,
    `scrub densify stayed busy too long after settle: ${JSON.stringify(report)}`,
  ).toBeLessThan(12);
});
