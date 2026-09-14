/**
 * Focused probe: cold settle must paint exact (eng == playhead).
 * Usage: npx playwright test e2e/perf.scrub.settle.spec.ts
 */
import { expect, test } from "@playwright/test";
import fs from "node:fs";
import path from "node:path";
import { DemoPlayer } from "./helpers/player";

const CLIP =
  process.env.WEBMEDIA_PERF_CLIP ??
  path.resolve(
    process.env.USERPROFILE ?? "",
    "Downloads",
    "WING IT - Blender Open Movie.mp4",
  );

test.describe.configure({ timeout: 120_000 });

test("cold scrub settle paints exact engine frame", async ({ page }) => {
  test.skip(!fs.existsSync(CLIP), `missing clip: ${CLIP}`);

  const player = new DemoPlayer(page);
  await player.goto();

  const openStart = Date.now();
  await player.fileInput.setInputFiles(CLIP);
  await player.waitReady(60_000);
  const openMs = Date.now() - openStart;

  // Single click near end — pointerdown + pointerup settle (no drag densify storm).
  const box = await player.scrubber.boundingBox();
  if (!box) throw new Error("no scrubber");
  const x = box.x + box.width * 0.85;
  const y = box.y + box.height / 2;
  await page.mouse.click(x, y);

  const settleStart = Date.now();
  let last = await player.readStatus();
  let settledMs: number | null = null;
  while (Date.now() - settleStart < 45_000) {
    await page.waitForTimeout(250);
    last = await player.readStatus();
    if (
      last.playhead != null &&
      last.engine != null &&
      last.playhead === last.engine &&
      last.playhead > 100
    ) {
      settledMs = Date.now() - settleStart;
      break;
    }
  }

  const snap = await player.snapshot();
  const report = {
    openMs,
    settledMs,
    timedOut: settledMs == null,
    status: snap.status.raw,
    playhead: snap.status.playhead,
    engine: snap.status.engine,
    pipeline: snap.pipeline,
    frameCache: snap.frameCache,
  };
  console.log(JSON.stringify(report, null, 2));
  await test.info().attach("settle-report", {
    body: JSON.stringify(report, null, 2),
    contentType: "application/json",
  });

  expect(settledMs, `exact not painted: ${JSON.stringify(report)}`).not.toBeNull();
  expect(snap.status.engine).toBe(snap.status.playhead);
  expect(snap.status.playhead!).toBeGreaterThan(1000);
});
