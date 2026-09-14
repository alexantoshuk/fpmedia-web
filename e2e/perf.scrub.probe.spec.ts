/**
 * One-off scrub performance probe against a local MP4.
 * Usage: npx playwright test e2e/perf.scrub.probe.spec.ts
 */
import { expect, test } from "@playwright/test";
import path from "node:path";
import { DemoPlayer } from "./helpers/player";

const CLIP =
  process.env.WEBMEDIA_PERF_CLIP ??
  path.resolve(
    process.env.USERPROFILE ?? "",
    "Downloads",
    "WING IT - Blender Open Movie.mp4",
  );

type Sample = {
  tMs: number;
  playhead: number | null;
  engine: number | null;
  seconds: number | null;
  status: string;
  pipeline: string;
  prefetchQueue: string;
  prefetchAhead: string;
  frameCache: string;
  scrubPct: number;
  dataPlayhead: number;
};

test.describe.configure({ timeout: 180_000 });

test("perf probe: Wing It scrub timeline", async ({ page }) => {
  test.skip(
    !(await import("node:fs")).existsSync(CLIP),
    `missing clip: ${CLIP}`,
  );

  const player = new DemoPlayer(page);
  const t0 = Date.now();
  const elapsed = () => Date.now() - t0;

  await player.goto();

  const openStart = Date.now();
  await player.fileInput.setInputFiles(CLIP);
  await player.waitReady(120_000);
  const openMs = Date.now() - openStart;
  const afterOpen = await player.snapshot();

  const sample = async (): Promise<Sample> => {
    const snap = await player.snapshot();
    return {
      tMs: elapsed(),
      playhead: snap.status.playhead,
      engine: snap.status.engine,
      seconds: snap.status.seconds,
      status: snap.status.raw,
      pipeline: snap.pipeline,
      prefetchQueue: snap.prefetchQueue,
      prefetchAhead: snap.prefetchAhead,
      frameCache: snap.frameCache,
      scrubPct: snap.scrubberPercent,
      dataPlayhead: snap.ariaPlayhead,
    };
  };

  /** Drag scrubber from fraction a→b over `steps` pointermoves. */
  const scrubDrag = async (from: number, to: number, steps: number) => {
    const box = await player.scrubber.boundingBox();
    if (!box) throw new Error("no scrubber box");
    const y = box.y + box.height / 2;
    const x = (f: number) => box.x + box.width * Math.min(1, Math.max(0, f));
    await page.mouse.move(x(from), y);
    await page.mouse.down();
    const samples: Sample[] = [];
    for (let i = 1; i <= steps; i++) {
      const f = from + ((to - from) * i) / steps;
      await page.mouse.move(x(f), y);
      if (i % Math.max(1, Math.floor(steps / 8)) === 0) {
        samples.push(await sample());
      }
    }
    await page.mouse.up();
    // settle window
    await page.waitForTimeout(150);
    samples.push(await sample());
    await page.waitForTimeout(500);
    samples.push(await sample());
    await page.waitForTimeout(1500);
    samples.push(await sample());
    return samples;
  };

  const waitSettled = async (timeout = 20_000) => {
    const start = Date.now();
    let last = await sample();
    while (Date.now() - start < timeout) {
      await page.waitForTimeout(200);
      const next = await sample();
      const ph = next.playhead;
      const eng = next.engine;
      // Exact painted — densify may still run (`dense` in cache detail).
      if (ph != null && eng != null && ph === eng) {
        return { settledMs: Date.now() - start, sample: next };
      }
      last = next;
    }
    return { settledMs: Date.now() - start, sample: last, timedOut: true };
  };

  const sweeps: {
    label: string;
    from: number;
    to: number;
    steps: number;
    during: Sample[];
    settle: Awaited<ReturnType<typeof waitSettled>>;
  }[] = [];

  for (const [label, from, to, steps] of [
    ["cold mid→near-end", 0.05, 0.85, 24],
    ["near-end→start", 0.85, 0.08, 24],
    ["start→quarter (densify)", 0.08, 0.35, 40],
    ["fast jump mid", 0.35, 0.55, 6],
  ] as const) {
    const during = await scrubDrag(from, to, steps);
    const settle = await waitSettled();
    sweeps.push({ label, from, to, steps, during, settle });
  }

  // Step stress near middle
  const mid = afterOpen.status.lastFrame
    ? Math.floor(afterOpen.status.lastFrame / 2)
    : 100;
  await player.seekToFrame(mid);
  const stepSamples: Sample[] = [await sample()];
  for (let i = 0; i < 10; i++) {
    await player.next.click();
    stepSamples.push(await sample());
  }

  const report = {
    clip: CLIP,
    openMs,
    afterOpen,
    sweeps: sweeps.map((s) => ({
      label: s.label,
      from: s.from,
      to: s.to,
      steps: s.steps,
      duringTail: s.during.slice(-3),
      duringAll: s.during,
      settleMs: s.settle.settledMs,
      settleTimedOut: "timedOut" in s.settle ? s.settle.timedOut : false,
      settleSample: s.settle.sample,
    })),
    stepSamples,
  };

  await test.info().attach("perf-scrub-report", {
    body: JSON.stringify(report, null, 2),
    contentType: "application/json",
  });

  // Also write to disk for agent analysis
  const out = path.resolve("test-results/perf-scrub-wing-it.json");
  await import("node:fs/promises").then((fs) =>
    fs.mkdir(path.dirname(out), { recursive: true }).then(() =>
      fs.writeFile(out, JSON.stringify(report, null, 2)),
    ),
  );

  console.log(
    JSON.stringify(
      {
        openMs,
        meta: afterOpen.meta,
        status: afterOpen.status.raw,
        frameCache: afterOpen.frameCache,
        sweeps: report.sweeps.map((s) => ({
          label: s.label,
          settleMs: s.settleMs,
          timedOut: s.settleTimedOut,
          endStatus: s.settleSample.status,
          endCache: s.settleSample.frameCache,
          endPipeline: s.settleSample.pipeline,
        })),
      },
      null,
      2,
    ),
  );

  expect(afterOpen.status.ready).toBe(true);
  expect(afterOpen.error).toBe("");
});
