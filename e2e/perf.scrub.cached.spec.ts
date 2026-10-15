/**
 * Cached-region scrub / step latency probe.
 *
 * Warms Exact densify, then measures next/prev and scrubber drag inside the
 * warm window (in-page timing via performance.now around Playwright actions).
 *
 * Usage (app on :3000):
 *   npx playwright test e2e/perf.scrub.cached.spec.ts --reporter=list
 */
import { expect, test } from "@playwright/test";
import fs from "node:fs";
import path from "node:path";
import { DemoPlayer, FIXTURES } from "./helpers/player";

function pct(sorted: number[], p: number): number {
  if (sorted.length === 0) return NaN;
  const i = Math.min(
    sorted.length - 1,
    Math.max(0, Math.ceil((p / 100) * sorted.length) - 1),
  );
  return sorted[i]!;
}

function stats(samples: number[]) {
  const sorted = [...samples].sort((a, b) => a - b);
  const sum = sorted.reduce((a, b) => a + b, 0);
  return {
    n: sorted.length,
    min: sorted[0] ?? NaN,
    p50: pct(sorted, 50),
    p90: pct(sorted, 90),
    p99: pct(sorted, 99),
    max: sorted[sorted.length - 1] ?? NaN,
    mean: sorted.length ? Number((sum / sorted.length).toFixed(1)) : NaN,
  };
}

const CLIP =
  process.env.WEBMEDIA_PERF_CLIP ??
  (fs.existsSync(FIXTURES.shortGop) ? FIXTURES.shortGop : "");

test.describe.configure({ timeout: 300_000 });

test("cached scrub latency", async ({ page }) => {
  test.skip(
    !CLIP || !fs.existsSync(CLIP),
    `missing in-repo fixture (demo-player/public/short_gop.mp4) or set WEBMEDIA_PERF_CLIP to an in-repo / testvideo-cached path`,
  );

  const consoleErrors: string[] = [];
  page.on("console", (msg) => {
    if (msg.type() === "error") consoleErrors.push(msg.text());
    const t = msg.text();
    if (
      t.includes("source image could not be decoded") ||
      t.includes("scrub blit miss")
    ) {
      consoleErrors.push(t);
    }
  });

  const player = new DemoPlayer(page);
  await player.goto();
  await player.openFixture(CLIP, 180_000);

  const afterOpen = await player.snapshot();
  const last = afterOpen.status.lastFrame ?? 0;
  expect(last).toBeGreaterThan(8);

  // Densify warm — large clips need longer.
  const warmTarget = Math.min(48, last + 1);
  const warmStart = Date.now();
  let warmFrames = await player.denseCacheFrames();
  while (Date.now() - warmStart < 90_000) {
    warmFrames = await player.denseCacheFrames();
    if (warmFrames >= warmTarget) break;
    await page.waitForTimeout(200);
  }
  const warmMs = Date.now() - warmStart;
  const warmSnap = await player.snapshot();
  expect(warmFrames, `densify did not warm: ${warmSnap.frameCache}`).toBeGreaterThanOrEqual(
    8,
  );

  // Seek into the middle of the warm window so next/prev stay cached.
  const warmLo = 0;
  const warmHi = Math.max(8, warmFrames - 1);
  const start = Math.min(warmLo + 2, warmHi - 4);
  const stepCount = Math.min(16, warmHi - start);

  await player.seekToFrame(start);
  // Allow densify to settle after seek.
  await page.waitForTimeout(400);

  // --- Next through warm region ---
  const nextMs: number[] = [];
  for (let i = 0; i < stepCount; i++) {
    const target = start + i + 1;
    const t0 = Date.now();
    await player.next.click();
    await player.waitEngine(target, 15_000);
    nextMs.push(Date.now() - t0);
  }

  // --- Prev back ---
  const prevMs: number[] = [];
  for (let i = 0; i < stepCount; i++) {
    const target = start + stepCount - 1 - i;
    const t0 = Date.now();
    await player.prev.click();
    await player.waitEngine(target, 15_000);
    prevMs.push(Date.now() - t0);
  }

  // --- Scrubber drag inside warm window ---
  const box = await player.scrubber.boundingBox();
  if (!box) throw new Error("no scrubber");
  const y = box.y + box.height / 2;
  const xAt = (f: number) => box.x + box.width * Math.min(1, Math.max(0, f));
  const warmFrac = Math.min(0.4, (warmFrames / Math.max(1, last)) * 0.85);

  await page.mouse.move(xAt(0.02), y);
  await page.mouse.down();
  const dragSamples: {
    t: number;
    engine: number | null;
    playhead: number | null;
  }[] = [];
  const dragSteps = 20;
  const dragT0 = Date.now();
  for (let i = 1; i <= dragSteps; i++) {
    const f = 0.02 + (warmFrac * i) / dragSteps;
    await page.mouse.move(xAt(f), y);
    const s = await player.readStatus();
    dragSamples.push({
      t: Date.now() - dragT0,
      engine: s.engine,
      playhead: s.playhead,
    });
  }
  await page.mouse.up();
  const dragMs = Date.now() - dragT0;

  const engineDeltas: number[] = [];
  for (let i = 1; i < dragSamples.length; i++) {
    const a = dragSamples[i - 1]!;
    const b = dragSamples[i]!;
    if (a.engine != null && b.engine != null && b.engine !== a.engine) {
      engineDeltas.push(b.t - a.t);
    }
  }

  // Reverse pass (same warm chunks — blit path).
  await page.mouse.move(xAt(warmFrac), y);
  await page.mouse.down();
  const pass2Eng: number[] = [];
  const p2t0 = Date.now();
  let prevEng = (await player.readStatus()).engine;
  for (let i = 1; i <= dragSteps; i++) {
    const f = warmFrac - ((warmFrac - 0.02) * i) / dragSteps;
    const tMove = Date.now();
    await page.mouse.move(xAt(f), y);
    // Wait briefly for engine to catch this move (cached blit should be fast).
    const deadline = Date.now() + 500;
    let eng = prevEng;
    while (Date.now() < deadline) {
      eng = (await player.readStatus()).engine;
      if (eng != null && eng !== prevEng) break;
      await page.waitForTimeout(8);
    }
    if (eng != null && eng !== prevEng) {
      pass2Eng.push(Date.now() - tMove);
      prevEng = eng;
    }
  }
  await page.mouse.up();
  const pass2Ms = Date.now() - p2t0;

  const report = {
    fixture: path.basename(CLIP),
    lastFrame: last,
    warm: { frames: warmFrames, ms: warmMs, cache: warmSnap.frameCache },
    nextStepMs: stats(nextMs),
    prevStepMs: stats(prevMs),
    dragPass1: {
      totalMs: dragMs,
      steps: dragSteps,
      engineChanges: engineDeltas.length,
      engineChangeDeltasMs: stats(engineDeltas),
    },
    dragPass2Cached: {
      totalMs: pass2Ms,
      engineMoves: pass2Eng.length,
      moveToEngineMs: stats(pass2Eng),
    },
    consoleErrors: consoleErrors.slice(0, 20),
    endCache: (await player.snapshot()).frameCache,
  };

  // eslint-disable-next-line no-console
  console.log("CACHED_SCRUB_REPORT\n" + JSON.stringify(report, null, 2));

  await test.info().attach("cached-scrub-report", {
    body: JSON.stringify(report, null, 2),
    contentType: "application/json",
  });

  expect(consoleErrors, consoleErrors.join("\n")).toEqual([]);
  // Soft budgets — cached Next/Prev should be blit-hot (≪ cold key→N decode).
  expect(report.nextStepMs.p50, "next p50").toBeLessThan(300);
  expect(report.prevStepMs.p50, "prev p50").toBeLessThan(300);
});
