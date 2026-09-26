/**
 * Fast scrub stress (~1 min): open → Pause→Play + scrub→Play latency →
 * rapid scrub across cache zones → Play→scrub→release→resume → report.
 *
 * Clip: WEBMEDIA_CLIP=gop12|gop48|fmp4|… | URL | local path (default gop48).
 *
 * Soft report always. Hard fail: open fail, unresponsive, no/slow Pause→Play,
 * scrub→Play, or resume after scrub (budget FAST_PLAY_MS).
 * Zone decode faults recorded; fail run with WEBMEDIA_STRESS_FAIL_ZONES=1.
 *
 *   npm run e2e:stress
 *   WEBMEDIA_CLIP=gop12 npm run e2e:stress
 */
import { expect, test, type Page } from "@playwright/test";
import {
  classifyFraction,
  DemoPlayer,
  grayCoverageRatio,
  type PipelineJson,
  type ScrubZone,
} from "./helpers/player";
import { resolveClip } from "./helpers/clip";

test.describe.configure({ timeout: 120_000 });

type SettleSample = {
  zone: ScrubZone | "combo";
  fraction: number;
  settleMs: number | null;
  playhead: number | null;
  engine: number | null;
  error: string;
};

function percentile(sorted: number[], p: number): number | null {
  if (sorted.length === 0) return null;
  const idx = Math.min(
    sorted.length - 1,
    Math.max(0, Math.ceil((p / 100) * sorted.length) - 1),
  );
  return sorted[idx]!;
}

function summarizeMs(values: Array<number | null | undefined>) {
  const xs = values
    .filter((v): v is number => typeof v === "number" && Number.isFinite(v))
    .sort((a, b) => a - b);
  return {
    n: xs.length,
    p50: percentile(xs, 50),
    p95: percentile(xs, 95),
    max: xs.length ? xs[xs.length - 1]! : null,
  };
}

async function assertResponsive(page: Page, ms = 5_000) {
  const ok = await Promise.race([
    page.evaluate(() => 1 + 1).then((v) => v === 2),
    page.waitForTimeout(ms).then(() => false),
  ]);
  expect(ok, `page unresponsive >${ms}ms`).toBe(true);
}

async function waitExactSettle(player: DemoPlayer, timeoutMs: number) {
  const t0 = Date.now();
  let last = await player.readStatus();
  while (Date.now() - t0 < timeoutMs) {
    last = await player.readStatus();
    if (
      last.playhead != null &&
      last.engine != null &&
      last.playhead === last.engine
    ) {
      return { settleMs: Date.now() - t0, status: last };
    }
    await player.page.waitForTimeout(50);
  }
  return { settleMs: null as number | null, status: last };
}

/** eng==playhead and pipeline marks Exact at that frame (not soft-cold settle). */
async function waitExactPainted(
  player: DemoPlayer,
  timeoutMs: number,
): Promise<{ settleMs: number | null; status: Awaited<ReturnType<DemoPlayer["readStatus"]>> }> {
  const t0 = Date.now();
  let last = await player.readStatus();
  while (Date.now() - t0 < timeoutMs) {
    last = await player.readStatus();
    if (
      last.playhead != null &&
      last.engine != null &&
      last.playhead === last.engine &&
      last.lastFrame != null
    ) {
      const pipe = await player.readPipelineJson();
      const frac = last.playhead / Math.max(1, last.lastFrame);
      if (classifyFraction(frac, pipe, last.lastFrame) === "exact") {
        return { settleMs: Date.now() - t0, status: last };
      }
    }
    await player.page.waitForTimeout(50);
  }
  return { settleMs: null, status: last };
}

/** Click Play already done — wait until playing with playhead past `ph0`. */
async function waitPlayAdvances(
  player: DemoPlayer,
  ph0: number,
  timeoutMs: number,
): Promise<number | null> {
  const t0 = Date.now();
  while (Date.now() - t0 < timeoutMs) {
    const s = await player.readStatus();
    if (s.playing && s.playhead != null && s.playhead > ph0) {
      return Date.now() - t0;
    }
    await player.page.waitForTimeout(40);
  }
  return null;
}

async function ensurePaused(player: DemoPlayer) {
  if ((await player.readStatus()).playing) {
    await player.play.click();
    await expect
      .poll(async () => (await player.readStatus()).playing, { timeout: 5_000 })
      .toBe(false);
  }
}

/** Play control enabled and not mid-play (densify may still run in bg). */
async function waitTransportIdle(player: DemoPlayer, timeoutMs = 15_000) {
  await expect(player.play).toBeEnabled({ timeout: timeoutMs });
  await expect
    .poll(async () => !(await player.readStatus()).playing, {
      timeout: timeoutMs,
      message: "still playing",
    })
    .toBe(true);
}

async function ensurePlaying(player: DemoPlayer, timeoutMs = 8_000) {
  if ((await player.readStatus()).playing) return;
  await player.play.click();
  await expect
    .poll(async () => (await player.readStatus()).playing, {
      timeout: timeoutMs,
    })
    .toBe(true);
}

/** Soft budget for warm Pause→Play / Exact-seeded scrub→Play (URL matrix). */
const FAST_PLAY_MS = 1_500;

function pickFracs(
  pipe: PipelineJson,
  lastFrame: number,
  zone: ScrubZone,
  n: number,
): number[] {
  const hits: number[] = [];
  for (let i = 1; i < 24; i++) {
    const f = i / 24;
    if (f < 0.1 || f > 0.9) continue;
    if (classifyFraction(f, pipe, lastFrame) === zone) hits.push(f);
  }
  if (hits.length === 0) return [];
  if (hits.length <= n) return hits;
  const step = hits.length / n;
  return Array.from({ length: n }, (_, i) => hits[Math.floor(i * step)]!);
}

test("stress: open → fast zone scrub → resume", async ({ page }) => {
  const clip = resolveClip();
  const strict = process.env.WEBMEDIA_STRESS_STRICT === "1";
  const failZones =
    strict || process.env.WEBMEDIA_STRESS_FAIL_ZONES === "1";
  const player = new DemoPlayer(page);
  const settleSamples: SettleSample[] = [];
  const resumeMsList: number[] = [];
  const resumePlayMsList: number[] = [];
  const pausePlayMsList: number[] = [];
  const scrubPlayMsList: number[] = [];
  const behindSamples: number[] = [];
  const faults: string[] = [];
  const warnings: string[] = [];

  page.on("pageerror", (err) => faults.push(`pageerror: ${err}`));
  const readError = async () => (await player.error.innerText()).trim();

  await player.goto();
  const openStart = Date.now();
  await player.openClip(clip, 60_000);
  const openMs = Date.now() - openStart;
  expect(await readError(), "open must succeed").toBe("");

  const status0 = await player.readStatus();
  const lastFrame = status0.lastFrame ?? 1;

  // Brief Play to seed gray / densify — then Pause.
  await player.play.click();
  await page.waitForTimeout(1_200);
  let pipe = await player.readPipelineJson();
  if ((await player.readStatus()).playing) {
    await player.play.click();
    await page.waitForTimeout(100);
  }

  // --- Pause → Play (warm soft-hold; must be fast) ---
  {
    await ensurePlaying(player);
    await page.waitForTimeout(400);
    await ensurePaused(player);
    await page.waitForTimeout(80);
    const ph0 = (await player.readStatus()).playhead ?? 0;
    await player.play.click();
    const ms = await waitPlayAdvances(player, ph0, 8_000);
    if (ms == null) {
      faults.push("Pause→Play did not start");
    } else {
      pausePlayMsList.push(ms);
      if (ms > FAST_PLAY_MS) {
        faults.push(`Pause→Play slow ${ms}ms (budget ${FAST_PLAY_MS})`);
      }
    }
    await ensurePaused(player);
    await waitTransportIdle(player);
  }

  // --- Paused Exact scrub → Play (settle seeds; PlayPrime skip fill) ---
  {
    await ensurePaused(player);
    pipe = await player.readPipelineJson();
    const frac =
      pickFracs(pipe, lastFrame, "gray", 1)[0] ??
      pickFracs(pipe, lastFrame, "exact", 1)[0] ??
      0.35;
    await player.scrubToFraction(frac);
    const painted = await waitExactPainted(player, 12_000);
    await assertResponsive(page, 5_000);
    await waitTransportIdle(player);
    if (painted.settleMs == null) {
      faults.push("scrub→Play: Exact paint timeout (soft-cold?)");
    } else {
      const ph0 = painted.status.playhead ?? 0;
      await player.play.click();
      const ms = await waitPlayAdvances(player, ph0, 8_000);
      if (ms == null) {
        faults.push("scrub→Play did not start");
      } else {
        scrubPlayMsList.push(ms);
        if (ms > FAST_PLAY_MS) {
          faults.push(`scrub→Play slow ${ms}ms (budget ${FAST_PLAY_MS})`);
        }
      }
    }
    await ensurePaused(player);
    await waitTransportIdle(player);
  }

  // --- Fast scrub: 1–2 taps per available zone (soft settle budget) ---
  const tap = async (zone: ScrubZone, fraction: number, budgetMs: number) => {
    await player.scrubToFraction(fraction);
    await assertResponsive(page, 5_000);
    const settled = await waitExactSettle(player, budgetMs);
    const err = await readError();
    settleSamples.push({
      zone,
      fraction,
      settleMs: settled.settleMs,
      playhead: settled.status.playhead,
      engine: settled.status.engine,
      error: err,
    });
    if (err) faults.push(`tap ${zone} @ ${fraction.toFixed(2)}: ${err}`);
    if (budgetMs >= 3_000 && settled.settleMs == null) {
      faults.push(`settle timeout ${zone} @ ${fraction.toFixed(2)}`);
    }
  };

  pipe = await player.readPipelineJson();
  const zones: ScrubZone[] = ["cold", "gray", "covered", "exact"];
  for (const zone of zones) {
    const fracs = pickFracs(pipe, lastFrame, zone, 2);
    // Cold: soft (2.5s). Warm zones: 6s settle max.
    const budget = zone === "cold" ? 2_500 : 6_000;
    for (const f of fracs) {
      await tap(zone, f, budget);
      if (await readError()) break;
    }
    pipe = await player.readPipelineJson();
  }
  // If zones were empty (e.g. fully gray local), still hammer mid timeline.
  if (settleSamples.length < 3) {
    for (const f of [0.2, 0.5, 0.8]) {
      await tap("gray", f, 6_000);
    }
  }

  // Sweep drag across bar (quick).
  {
    const box = await player.scrubber.boundingBox();
    if (box) {
      const y = box.y + box.height / 2;
      await page.mouse.move(box.x + box.width * 0.1, y);
      await page.mouse.down();
      for (let i = 1; i <= 12; i++) {
        await page.mouse.move(box.x + box.width * (0.1 + (0.8 * i) / 12), y);
      }
      await page.mouse.up();
      await assertResponsive(page, 5_000);
      await waitExactSettle(player, 6_000);
    }
  }

  // --- Combo: Play → scrub → release → resume (once) ---
  await waitTransportIdle(player, 20_000);
  await player.scrubToFraction(0.25);
  await waitExactSettle(player, 6_000);
  await player.play.click();

  const playDeadline = Date.now() + 8_000;
  let playing = false;
  while (Date.now() < playDeadline) {
    const s = await player.readStatus();
    if (s.playing) {
      playing = true;
      break;
    }
    await page.waitForTimeout(100);
  }
  if (!playing) {
    faults.push("Play did not start");
  } else {
    await page.waitForTimeout(400);
    const box = await player.scrubber.boundingBox();
    if (!box) throw new Error("no scrubber");
    const y = box.y + box.height / 2;
    const x = (f: number) => box.x + box.width * Math.min(1, Math.max(0, f));

    await page.mouse.move(x(0.25), y);
    await page.mouse.down();
    for (let i = 1; i <= 8; i++) {
      await page.mouse.move(x(0.25 + (0.35 * i) / 8), y);
    }
    const beforeUp = await player.readStatus();
    const resumeStart = Date.now();
    await page.mouse.up();

    const settled = await waitExactSettle(player, 8_000);
    const err = await readError();
    settleSamples.push({
      zone: "combo",
      fraction: 0.6,
      settleMs: settled.settleMs,
      playhead: settled.status.playhead,
      engine: settled.status.engine,
      error: err,
    });
    if (err) faults.push(`combo scrub: ${err}`);

    const ph0 = settled.status.playhead ?? beforeUp.playhead ?? 0;
    const playStart = Date.now();
    let resumed: number | null = null;
    let resumePlay: number | null = null;
    while (Date.now() - resumeStart < 10_000) {
      const s = await player.readStatus();
      const pipeNow = await player.readPipelineJson();
      if (pipeNow.last_behind_ms != null) behindSamples.push(pipeNow.last_behind_ms);
      if (
        (s.playing && s.playhead != null && s.playhead > ph0) ||
        (s.playhead != null && s.playhead > ph0 + 2)
      ) {
        resumed = Date.now() - resumeStart;
        resumePlay = Date.now() - playStart;
        break;
      }
      if (!s.playing && Date.now() - resumeStart > 1_500) {
        await player.play.click();
        // Reset playStart so we measure click→advance, not settle wait.
        // (playStart already past settle; optional re-click is recovery.)
      }
      await page.waitForTimeout(80);
    }
    if (resumed == null) faults.push(`no resume after scrub (ph0=${ph0})`);
    else {
      resumeMsList.push(resumed);
      if (resumePlay != null) {
        resumePlayMsList.push(resumePlay);
        if (resumePlay > FAST_PLAY_MS) {
          faults.push(
            `resume Play slow ${resumePlay}ms after settle (budget ${FAST_PLAY_MS})`,
          );
        }
      }
    }
  }

  const byZone = (zone: ScrubZone | "combo") =>
    summarizeMs(
      settleSamples.filter((s) => s.zone === zone).map((s) => s.settleMs),
    );

  const report = {
    clip: {
      raw: clip.raw,
      kind: clip.kind,
      value: clip.value,
      label: clip.label,
    },
    openMs,
    grayCoverageEnd: grayCoverageRatio(await player.readPipelineJson()),
    settle: {
      cold: byZone("cold"),
      gray: byZone("gray"),
      covered: byZone("covered"),
      exact: byZone("exact"),
      combo: byZone("combo"),
      all: summarizeMs(settleSamples.map((s) => s.settleMs)),
    },
    resumeMs: summarizeMs(resumeMsList),
    resumePlayMs: summarizeMs(resumePlayMsList),
    pausePlayMs: summarizeMs(pausePlayMsList),
    scrubPlayMs: summarizeMs(scrubPlayMsList),
    behindMs: summarizeMs(behindSamples),
    samples: { settle: settleSamples.length },
    faults,
    warnings,
    failZones,
    strict,
    fastPlayBudgetMs: FAST_PLAY_MS,
  };

  const flush = async () => {
    console.log(JSON.stringify(report, null, 2));
    const reportPath = process.env.WEBMEDIA_STRESS_REPORT_PATH;
    if (reportPath) {
      const fs = await import("node:fs/promises");
      const pathMod = await import("node:path");
      await fs.mkdir(pathMod.dirname(reportPath), { recursive: true });
      await fs.writeFile(reportPath, JSON.stringify(report, null, 2), "utf8");
    }
    await test.info().attach("stress-scrub-av-report", {
      body: JSON.stringify(report, null, 2),
      contentType: "application/json",
    });
  };

  try {
    expect(
      report.resumeMs.n,
      `no resume: ${faults.join(" | ")}`,
    ).toBeGreaterThan(0);
    expect(
      report.pausePlayMs.n,
      `no Pause→Play sample: ${faults.join(" | ")}`,
    ).toBeGreaterThan(0);
    expect(
      report.scrubPlayMs.n,
      `no scrub→Play sample: ${faults.join(" | ")}`,
    ).toBeGreaterThan(0);
    if (failZones) {
      expect(faults, faults.join("\n")).toEqual([]);
    } else if (faults.length) {
      warnings.push(`zone faults (non-strict): ${faults.join(" | ")}`);
    }
    const fatal = faults.filter(
      (f) =>
        f.includes("no resume") ||
        f.includes("Play did not start") ||
        f.includes("Pause→Play did not start") ||
        f.includes("scrub→Play did not start") ||
        f.includes("scrub→Play: Exact paint") ||
        f.includes("Pause→Play slow") ||
        f.includes("scrub→Play slow") ||
        f.includes("resume Play slow") ||
        f.startsWith("pageerror:"),
    );
    expect(fatal, fatal.join("\n")).toEqual([]);
    if (strict) {
      expect(report.resumeMs.p95).toBeLessThan(5_000);
      if (report.behindMs.p95 != null) {
        expect(report.behindMs.p95).toBeLessThan(750);
      }
    }
  } finally {
    await flush();
  }
});
