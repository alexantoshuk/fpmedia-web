/**
 * Scrub perf baseline capture + compare.
 *
 * Modes (WEBMEDIA_BASELINE_MODE):
 *   save    — write JSON under e2e/baselines/<tag>/
 *   compare — re-run identical scenarios and fail on regressions
 *
 * Clips: testvideo Release assets (see `helpers/testvideo.ts`). Re-save after
 * changing clip ids or scenario defs.
 *
 * Env:
 *   WEBMEDIA_BASELINE_TAG   default pre-decode-sink
 *   WEBMEDIA_SETTLE_BUDGET  max settleMs / baseline.settleMs (default 1.75)
 *   WEBMEDIA_DRAG_BUDGET    max dragMs / baseline.dragMs (default 2.0)
 *   WEBMEDIA_SETTLE_SLACK_MS absolute slack added to settle budget (default 2000)
 */
import fs from "node:fs";
import fsp from "node:fs/promises";
import path from "node:path";
import type { Page } from "@playwright/test";
import { DemoPlayer, type PlayerStatus } from "./player";
import {
  TestvideoClip,
  TestvideoMeta,
  testvideoUrl,
} from "./testvideo";

export const BASELINE_TAG =
  process.env.WEBMEDIA_BASELINE_TAG ?? "pre-decode-sink";

export type BaselineMode = "save" | "compare";

export function baselineMode(): BaselineMode {
  const raw = (process.env.WEBMEDIA_BASELINE_MODE ?? "save").toLowerCase();
  return raw === "compare" ? "compare" : "save";
}

export type ClipId =
  | typeof TestvideoClip.gop12
  | typeof TestvideoClip.gop48;

export type ClipDef = {
  id: ClipId;
  label: string;
  url: string;
};

export const PERF_CLIPS: ClipDef[] = [
  {
    id: TestvideoClip.gop12,
    label: TestvideoMeta[TestvideoClip.gop12],
    url: testvideoUrl(TestvideoClip.gop12),
  },
  {
    id: TestvideoClip.gop48,
    label: TestvideoMeta[TestvideoClip.gop48],
    url: testvideoUrl(TestvideoClip.gop48),
  },
];

/** Fixed scrub scenarios — keep identical across save/compare. */
export type ScrubScenarioDef = {
  id: string;
  label: string;
  from: number;
  to: number;
  /** Pointermove count during the drag. */
  steps: number;
  /** Delay after each move (0 = as fast as Playwright allows). */
  stepDelayMs: number;
};

export const SCRUB_SCENARIOS: ScrubScenarioDef[] = [
  {
    id: "fast-large",
    label: "fast scrub across large range",
    from: 0.12,
    to: 0.78,
    steps: 8,
    stepDelayMs: 0,
  },
  {
    id: "slow-small",
    label: "slow scrub on small range",
    from: 0.42,
    to: 0.5,
    steps: 36,
    stepDelayMs: 45,
  },
  {
    id: "slow-large",
    label: "slow scrub across large range",
    from: 0.1,
    to: 0.72,
    steps: 48,
    stepDelayMs: 35,
  },
];

export type Sample = {
  tMs: number;
  playhead: number | null;
  engine: number | null;
  seconds: number | null;
  status: string;
  pipeline: string;
  frameCache: string;
  scrubPct: number;
};

export type ScenarioResult = {
  id: string;
  label: string;
  from: number;
  to: number;
  steps: number;
  stepDelayMs: number;
  dragMs: number;
  settleMs: number;
  settleTimedOut: boolean;
  exactOk: boolean;
  endPlayhead: number | null;
  endEngine: number | null;
  endStatus: string;
  endPipeline: string;
  endFrameCache: string;
  /** Sparse samples during drag + settle (for diff diagnosis). */
  samples: Sample[];
};

export type ClipBaseline = {
  version: 1;
  tag: string;
  clipId: ClipId;
  clipLabel: string;
  /** testvideo asset filename, e.g. `avc_1080p_24_gop48_bf0.mp4`. */
  clipAsset: string;
  createdAt: string;
  openMs: number;
  afterOpen: {
    status: string;
    meta: string;
    lastFrame: number | null;
    frameCache: string;
  };
  scenarios: ScenarioResult[];
};

export type CompareFinding = {
  scenarioId: string;
  metric: string;
  baseline: number | boolean | string;
  actual: number | boolean | string;
  detail: string;
};

function baselinesDir(tag = BASELINE_TAG): string {
  return path.resolve("e2e/baselines", tag);
}

export function baselinePath(clipId: ClipId, tag = BASELINE_TAG): string {
  return path.join(baselinesDir(tag), `${clipId}.json`);
}

async function sampleAt(
  player: DemoPlayer,
  t0: number,
): Promise<Sample> {
  const snap = await player.snapshot();
  return {
    tMs: Date.now() - t0,
    playhead: snap.status.playhead,
    engine: snap.status.engine,
    seconds: snap.status.seconds,
    status: snap.status.raw,
    pipeline: snap.pipeline,
    frameCache: snap.frameCache,
    scrubPct: snap.scrubberPercent,
  };
}

async function scrubDrag(
  page: Page,
  player: DemoPlayer,
  scenario: ScrubScenarioDef,
  t0: number,
): Promise<{ dragMs: number; samples: Sample[] }> {
  const box = await player.scrubber.boundingBox();
  if (!box) throw new Error("no scrubber box");
  const y = box.y + box.height / 2;
  const x = (f: number) =>
    box.x + box.width * Math.min(1, Math.max(0, f));

  const samples: Sample[] = [];
  const dragStart = Date.now();
  await page.mouse.move(x(scenario.from), y);
  await page.mouse.down();

  const sampleEvery = Math.max(1, Math.floor(scenario.steps / 6));
  for (let i = 1; i <= scenario.steps; i++) {
    const f =
      scenario.from +
      ((scenario.to - scenario.from) * i) / scenario.steps;
    await page.mouse.move(x(f), y);
    if (scenario.stepDelayMs > 0) {
      await page.waitForTimeout(scenario.stepDelayMs);
    }
    if (i % sampleEvery === 0 || i === scenario.steps) {
      samples.push(await sampleAt(player, t0));
    }
  }

  await page.mouse.up();
  const dragMs = Date.now() - dragStart;
  samples.push(await sampleAt(player, t0));
  return { dragMs, samples };
}

async function waitExact(
  player: DemoPlayer,
  t0: number,
  timeoutMs = 120_000,
): Promise<{
  settleMs: number;
  timedOut: boolean;
  sample: Sample;
  status: PlayerStatus;
}> {
  const start = Date.now();
  let last = await sampleAt(player, t0);
  let status = await player.readStatus();
  while (Date.now() - start < timeoutMs) {
    await player.page.waitForTimeout(200);
    status = await player.readStatus();
    last = await sampleAt(player, t0);
    if (
      status.playhead != null &&
      status.engine != null &&
      status.playhead === status.engine
    ) {
      return {
        settleMs: Date.now() - start,
        timedOut: false,
        sample: last,
        status,
      };
    }
  }
  return {
    settleMs: Date.now() - start,
    timedOut: true,
    sample: last,
    status,
  };
}

/** After a densify storm, give scrub fill a moment to yield before timing settle. */
async function waitDensifyQuiet(player: DemoPlayer, maxMs = 8_000) {
  const start = Date.now();
  while (Date.now() - start < maxMs) {
    if (!(await player.scrubBusy())) return;
    await player.page.waitForTimeout(150);
  }
}

export async function runClipBaseline(
  page: Page,
  clip: ClipDef,
): Promise<ClipBaseline> {
  const player = new DemoPlayer(page);
  const t0 = Date.now();
  await player.goto();

  const openStart = Date.now();
  await player.openUrl(clip.url, 180_000);
  const openMs = Date.now() - openStart;
  const afterOpenSnap = await player.snapshot();

  const scenarios: ScenarioResult[] = [];
  for (const def of SCRUB_SCENARIOS) {
    const { dragMs, samples: dragSamples } = await scrubDrag(
      page,
      player,
      def,
      t0,
    );
    // Densify may still own the scrub decoder; settle timing starts after quiet.
    await waitDensifyQuiet(player);
    const settle = await waitExact(player, t0);
    const samples = [...dragSamples, settle.sample];
    scenarios.push({
      id: def.id,
      label: def.label,
      from: def.from,
      to: def.to,
      steps: def.steps,
      stepDelayMs: def.stepDelayMs,
      dragMs,
      settleMs: settle.settleMs,
      settleTimedOut: settle.timedOut,
      exactOk:
        !settle.timedOut && settle.status.playhead === settle.status.engine,
      endPlayhead: settle.status.playhead,
      endEngine: settle.status.engine,
      endStatus: settle.sample.status,
      endPipeline: settle.sample.pipeline,
      endFrameCache: settle.sample.frameCache,
      samples,
    });

    // Recover playhead if settle wedged so the next scenario starts clean.
    if (settle.timedOut) {
      await player.scrubToFraction(0.05);
      await waitDensifyQuiet(player, 5_000);
      await waitExact(player, t0, 60_000);
    }
  }

  return {
    version: 1,
    tag: BASELINE_TAG,
    clipId: clip.id,
    clipLabel: clip.label,
    clipAsset: `${clip.id}.mp4`,
    createdAt: new Date().toISOString(),
    openMs,
    afterOpen: {
      status: afterOpenSnap.status.raw,
      meta: afterOpenSnap.meta,
      lastFrame: afterOpenSnap.status.lastFrame,
      frameCache: afterOpenSnap.frameCache,
    },
    scenarios,
  };
}

export async function saveBaseline(report: ClipBaseline): Promise<string> {
  const out = baselinePath(report.clipId, report.tag);
  await fsp.mkdir(path.dirname(out), { recursive: true });
  await fsp.writeFile(out, JSON.stringify(report, null, 2));
  return out;
}

export async function loadBaseline(
  clipId: ClipId,
  tag = BASELINE_TAG,
): Promise<ClipBaseline> {
  const p = baselinePath(clipId, tag);
  const raw = await fsp.readFile(p, "utf8");
  return JSON.parse(raw) as ClipBaseline;
}

function numBudget(
  actual: number,
  baseline: number,
  ratio: number,
  slackMs: number,
): boolean {
  if (baseline <= 0) return actual <= slackMs;
  return actual <= baseline * ratio + slackMs;
}

export function compareBaselines(
  baseline: ClipBaseline,
  actual: ClipBaseline,
): CompareFinding[] {
  const settleRatio = Number(process.env.WEBMEDIA_SETTLE_BUDGET ?? "1.75");
  const dragRatio = Number(process.env.WEBMEDIA_DRAG_BUDGET ?? "2.0");
  const settleSlack = Number(process.env.WEBMEDIA_SETTLE_SLACK_MS ?? "2000");
  const dragSlack = Number(process.env.WEBMEDIA_DRAG_SLACK_MS ?? "3000");

  const findings: CompareFinding[] = [];

  if (baseline.clipId !== actual.clipId) {
    findings.push({
      scenarioId: "*",
      metric: "clipId",
      baseline: baseline.clipId,
      actual: actual.clipId,
      detail: "clip id mismatch",
    });
  }

  for (const b of baseline.scenarios) {
    const a = actual.scenarios.find((s) => s.id === b.id);
    if (!a) {
      findings.push({
        scenarioId: b.id,
        metric: "missing",
        baseline: true,
        actual: false,
        detail: "scenario missing from actual run",
      });
      continue;
    }

    if (a.settleTimedOut || !a.exactOk) {
      if (b.exactOk && !b.settleTimedOut) {
        findings.push({
          scenarioId: b.id,
          metric: "exactOk",
          baseline: b.exactOk,
          actual: a.exactOk,
          detail: `settle failed (timedOut=${a.settleTimedOut}, ph=${a.endPlayhead}, eng=${a.endEngine})`,
        });
      }
      // Baseline also missed exact — don't treat as new exact regression.
      continue;
    }

    if (!numBudget(a.settleMs, b.settleMs, settleRatio, settleSlack)) {
      findings.push({
        scenarioId: b.id,
        metric: "settleMs",
        baseline: b.settleMs,
        actual: a.settleMs,
        detail: `settle regression (> ${settleRatio}x + ${settleSlack}ms)`,
      });
    }

    if (!numBudget(a.dragMs, b.dragMs, dragRatio, dragSlack)) {
      findings.push({
        scenarioId: b.id,
        metric: "dragMs",
        baseline: b.dragMs,
        actual: a.dragMs,
        detail: `drag regression (> ${dragRatio}x + ${dragSlack}ms)`,
      });
    }
  }

  return findings;
}

/** Write a side-by-side compare artifact (always, even on pass). */
export async function saveCompareReport(
  clipId: ClipId,
  baseline: ClipBaseline,
  actual: ClipBaseline,
  findings: CompareFinding[],
): Promise<string> {
  const dir = path.resolve("test-results/baseline-compare");
  await fsp.mkdir(dir, { recursive: true });
  const out = path.join(dir, `${clipId}-${BASELINE_TAG}.json`);
  await fsp.writeFile(
    out,
    JSON.stringify(
      {
        tag: BASELINE_TAG,
        clipId,
        findings,
        summary: actual.scenarios.map((a) => {
          const b = baseline.scenarios.find((s) => s.id === a.id);
          return {
            id: a.id,
            baselineSettleMs: b?.settleMs ?? null,
            actualSettleMs: a.settleMs,
            baselineDragMs: b?.dragMs ?? null,
            actualDragMs: a.dragMs,
            exactOk: a.exactOk,
          };
        }),
        actual,
      },
      null,
      2,
    ),
  );
  return out;
}
