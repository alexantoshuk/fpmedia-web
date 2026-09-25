/**
 * Super scrub stress: rapid taps across cold / gray / Covered / Exact zones,
 * then Play→scrub→release→resume→scrub again. Collects latency stats.
 *
 * Clip (not hardcoded):
 *   WEBMEDIA_CLIP=gop48|gop12|fmp4|… | https://…/x.mp4 | C:\path\to\local.mp4
 * Default: gop48.
 *
 * Soft report always (JSON attach). Hard fail: unresponsive page, open fail,
 * Play dead even after reopen, no successful scrub→resume.
 * Zone decode/settle faults are recorded always; fail the run when
 * `WEBMEDIA_STRESS_FAIL_ZONES=1` or `WEBMEDIA_STRESS_STRICT=1` (also lag budgets).
 * Cold taps are soft-settle only (exact cascade on cold long-GOP is a known
 * hang vector — see `player.scrub.cold-open.spec.ts`).
 *
 *   npm run e2e:stress
 *   WEBMEDIA_CLIP=gop12 npm run e2e:stress
 *   WEBMEDIA_STRESS_FAIL_ZONES=1 npm run e2e:stress
 */
import { expect, test, type Page } from "@playwright/test";
import {
  classifyFraction,
  DemoPlayer,
  grayCoverageRatio,
  pickZoneFraction,
  type PipelineJson,
  type ScrubZone,
} from "./helpers/player";
import { resolveClip, type ResolvedClip } from "./helpers/clip";

test.describe.configure({ timeout: 420_000 });

type SettleSample = {
  zone: ScrubZone | "combo" | "unknown";
  fraction: number;
  settleMs: number | null;
  soft: boolean;
  playhead: number | null;
  engine: number | null;
  error: string;
};

type PlaySample = {
  tMs: number;
  playhead: number | null;
  engine: number | null;
  behindMs: number | null;
  holds: number;
  holdMs: number;
  skipped: number;
  playing: boolean;
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

async function assertResponsive(page: Page, ms = 8_000) {
  const ok = await Promise.race([
    page.evaluate(() => 1 + 1).then((v) => v === 2),
    page.waitForTimeout(ms).then(() => false),
  ]);
  expect(ok, `page unresponsive >${ms}ms`).toBe(true);
}

async function waitExactSettle(
  player: DemoPlayer,
  timeoutMs: number,
): Promise<{
  settleMs: number | null;
  status: Awaited<ReturnType<DemoPlayer["readStatus"]>>;
}> {
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
    await player.page.waitForTimeout(100);
  }
  return { settleMs: null, status: last };
}

test("stress: zone scrub + play/scrub/resume latency", async ({ page }) => {
  const clip = resolveClip();
  const strict = process.env.WEBMEDIA_STRESS_STRICT === "1";
  const player = new DemoPlayer(page);
  const settleSamples: SettleSample[] = [];
  const playSamples: PlaySample[] = [];
  const resumeMsList: number[] = [];
  const faults: string[] = [];
  const warnings: string[] = [];

  page.on("pageerror", (err) => faults.push(`pageerror: ${err}`));

  const readError = async () => (await player.error.innerText()).trim();

  const noteError = async (ctx: string) => {
    const err = await readError();
    if (err) faults.push(`${ctx}: ${err}`);
    return err;
  };

  /** After a sticky decode fault or dead transport, reopen for later phases. */
  const recoverIfNeeded = async (
    ctx: string,
    opened: ResolvedClip,
    force = false,
  ) => {
    const err = await readError();
    if (!force && !err) return opened;
    // Tap path already recorded the decode fault — reopen is recovery, not a new fault.
    if (err) warnings.push(`${ctx}: reopen after «${err}»`);
    else if (force) warnings.push(`${ctx}: forced reopen (Play/transport stuck)`);
    await player.goto();
    const again = await player.openClip(clip, 180_000);
    const after = await readError();
    if (after) faults.push(`reopen still errored: ${after}`);
    return again;
  };

  const samplePlay = async (t0: number): Promise<PlaySample> => {
    const status = await player.readStatus();
    const pipe = await player.readPipelineJson();
    return {
      tMs: Date.now() - t0,
      playhead: status.playhead,
      engine: status.engine,
      behindMs: pipe.last_behind_ms,
      holds: pipe.underrun_holds,
      holdMs: pipe.underrun_hold_ms,
      skipped: pipe.frames_skipped,
      playing: status.playing,
    };
  };

  const waitQuiet = async (ms = 8_000) => {
    const deadline = Date.now() + ms;
    while (Date.now() < deadline) {
      const pipeBusy = await player.readPipelineJson();
      if (!pipeBusy.scrub_busy) return;
      await page.waitForTimeout(100);
    }
  };

  /**
   * @param requireExact — warm zones must paint eng==playhead. Cold is soft
   *   (record settle if it lands; never force key→N on uncached long-GOP).
   */
  const rapidTaps = async (
    zone: ScrubZone,
    fractions: number[],
    settleBudgetMs: number,
    requireExact: boolean,
  ) => {
    for (const fraction of fractions) {
      await waitQuiet(8_000);
      await player.scrubToFraction(fraction);
      await assertResponsive(page, 8_000);

      let settleMs: number | null = null;
      let status = await player.readStatus();
      if (requireExact) {
        const settled = await waitExactSettle(player, settleBudgetMs);
        settleMs = settled.settleMs;
        status = settled.status;
        if (settleMs == null) {
          faults.push(
            `settle timeout zone=${zone} f=${fraction}: eng=${status.engine} ph=${status.playhead}`,
          );
        }
      } else {
        // Soft: brief poll only — success if exact lands early; no fail if cold.
        const softBudget = Math.min(2_500, settleBudgetMs);
        const settled = await waitExactSettle(player, softBudget);
        settleMs = settled.settleMs;
        status = settled.status;
        await page.waitForTimeout(40);
      }

      const err = await readError();
      settleSamples.push({
        zone,
        fraction,
        settleMs,
        soft: !requireExact,
        playhead: status.playhead,
        engine: status.engine,
        error: err,
      });
      if (err) {
        faults.push(`tap ${zone} @ ${fraction.toFixed(3)}: ${err}`);
        // Sticky closed codec — stop hammering this zone.
        break;
      }
    }
  };

  const pickFracs = (
    pipe: PipelineJson,
    lastFrame: number,
    zone: ScrubZone,
    n: number,
  ): number[] => {
    const out: number[] = [];
    const seen = new Set<string>();
    for (let i = 1; i < 48 && out.length < n * 3; i++) {
      const f = i / 48;
      if (f < 0.12 || f > 0.88) continue;
      if (classifyFraction(f, pipe, lastFrame) !== zone) continue;
      const key = f.toFixed(3);
      if (seen.has(key)) continue;
      seen.add(key);
      out.push(f);
    }
    if (out.length === 0) return [];
    if (out.length <= n) return out;
    const step = out.length / n;
    const picked: number[] = [];
    for (let i = 0; i < n; i++) {
      picked.push(out[Math.floor(i * step)]!);
    }
    return picked;
  };

  await player.goto();

  const openStart = Date.now();
  let opened = await player.openClip(clip, 180_000);
  const openMs = Date.now() - openStart;
  await noteError("after open");
  expect(await readError(), "open must succeed").toBe("");

  let status = await player.readStatus();
  const lastFrame = status.lastFrame ?? 1;
  let pipe = await player.readPipelineJson();

  // --- Phase: cold rapid scrub (soft-drag + short taps; no Exact settle wait) ---
  {
    const box = await player.scrubber.boundingBox();
    if (!box) throw new Error("no scrubber");
    const y = box.y + box.height / 2;
    // Soft-drag sweep while bytes may still be cold (cold-open pattern).
    await page.mouse.move(box.x + 2, y);
    await page.mouse.down();
    const steps = 32;
    for (let i = 0; i <= steps; i++) {
      await page.mouse.move(box.x + (box.width * i) / steps, y);
      if (i % 8 === 0) await assertResponsive(page, 8_000);
      await page.waitForTimeout(25);
    }
    await page.mouse.up();
    await assertResponsive(page, 8_000);
    // Quick taps — soft poll only (do not require eng==playhead on cold).
    const coldFracs = pickFracs(pipe, lastFrame, "cold", 4);
    const fracs =
      coldFracs.length >= 2 ? coldFracs.slice(0, 4) : [0.2, 0.5, 0.8];
    await rapidTaps("cold", fracs, 2_500, false);
    opened = await recoverIfNeeded("after cold", opened);
    status = await player.readStatus();
  }

  // --- Phase: warm gray via Play ---
  {
    await expect(player.play).toBeEnabled({ timeout: 15_000 });
    await player.play.click();
    const warmT0 = Date.now();
    while (Date.now() - warmT0 < 12_000) {
      playSamples.push(await samplePlay(warmT0));
      pipe = await player.readPipelineJson();
      if (grayCoverageRatio(pipe) >= 0.12) break;
      await page.waitForTimeout(200);
      await assertResponsive(page, 8_000);
    }
    status = await player.readStatus();
    if (status.playing) {
      await player.play.click();
      await expect
        .poll(async () => (await player.readStatus()).playing, {
          timeout: 5_000,
        })
        .toBe(false);
    }
    await noteError("after gray warm");
    opened = await recoverIfNeeded("after gray warm", opened);
  }

  // --- Phase: gray-only rapid taps (exact settle) ---
  {
    pipe = await player.readPipelineJson();
    const grayFracs = pickFracs(pipe, lastFrame, "gray", 5);
    const fracs =
      grayFracs.length >= 2 ? grayFracs : [0.25, 0.45, 0.65];
    await rapidTaps("gray", fracs, 45_000, true);
    opened = await recoverIfNeeded("after gray scrub", opened);
  }

  // --- Phase: wait for Covered / Exact, then scrub those zones ---
  {
    const densifyT0 = Date.now();
    while (Date.now() - densifyT0 < 60_000) {
      pipe = await player.readPipelineJson();
      const coveredFrac = pickZoneFraction(pipe, lastFrame, "covered");
      const exactFrac = pickZoneFraction(pipe, lastFrame, "exact");
      if (coveredFrac != null && exactFrac != null) break;
      if (Date.now() - densifyT0 > 3_000 && coveredFrac == null) {
        const box = await player.scrubber.boundingBox();
        if (box) {
          const y = box.y + box.height / 2;
          const x = (f: number) => box.x + box.width * f;
          await page.mouse.move(x(0.2), y);
          await page.mouse.down();
          for (let i = 1; i <= 12; i++) {
            await page.mouse.move(x(0.2 + (0.25 * i) / 12), y);
          }
          await page.mouse.up();
          await waitExactSettle(player, 30_000);
          await assertResponsive(page, 8_000);
        }
      }
      await page.waitForTimeout(250);
    }

    pipe = await player.readPipelineJson();
    const coveredHits = pickFracs(pipe, lastFrame, "covered", 3);
    const exactHits = pickFracs(pipe, lastFrame, "exact", 3);
    await waitQuiet(10_000);
    await page.waitForTimeout(300);
    // Densify storm can leave decodeQ=41 / stalled pump — reopen before Exact hammers.
    if (await readError()) {
      opened = await recoverIfNeeded("before green scrub", opened);
    }
    if (coveredHits.length > 0) {
      await rapidTaps("covered", coveredHits, 45_000, true);
    }
    if (exactHits.length > 0 && !(await readError())) {
      await rapidTaps("exact", exactHits, 30_000, true);
    }
    opened = await recoverIfNeeded("after green scrub", opened);
  }

  // --- Phase: combo Play → scrub → resume → scrub → resume ---
  {
    // Fresh open — zone hammers often leave Settling/busy with no error text.
    opened = await recoverIfNeeded("before combo", opened, true);
    await waitQuiet(10_000);
    await expect(player.play).toBeEnabled({ timeout: 15_000 });
    // Park mid-timeline so resume has room to advance.
    await player.scrubToFraction(0.2);
    await waitExactSettle(player, 30_000);
    await waitQuiet(5_000);
    await noteError("before combo play");

    let comboT0 = Date.now();
    await player.play.click();
    let started = false;
    const waitPlaying = async (ms: number) => {
      const t0 = Date.now();
      const ph0 = (await player.readStatus()).playhead ?? 0;
      while (Date.now() - t0 < ms) {
        const s = await player.readStatus();
        if (s.playing) return true;
        // Some hosts flip playing late — advancing playhead counts.
        if (s.playhead != null && s.playhead > ph0 + 2) return true;
        await page.waitForTimeout(150);
      }
      return false;
    };
    started = await waitPlaying(45_000);
    if (!started) {
      warnings.push("combo: Play did not start within 45s — reopening once more");
      opened = await recoverIfNeeded("combo play start", opened, true);
      await waitQuiet(5_000);
      await expect(player.play).toBeEnabled({ timeout: 15_000 });
      await player.scrubToFraction(0.2);
      await waitExactSettle(player, 30_000);
      comboT0 = Date.now();
      await player.play.click();
      started = await waitPlaying(45_000);
      if (!started) faults.push("combo: Play still dead after reopen");
    }

    if (started) {
      await page.waitForTimeout(800);
      playSamples.push(await samplePlay(comboT0));

      const box = await player.scrubber.boundingBox();
      if (!box) throw new Error("no scrubber");
      const y = box.y + box.height / 2;
      const x = (f: number) =>
        box.x + box.width * Math.min(1, Math.max(0, f));

      const runScrubReleaseResume = async (
        from: number,
        to: number,
        steps: number,
      ) => {
        await page.mouse.move(x(from), y);
        await page.mouse.down();
        for (let i = 1; i <= steps; i++) {
          await page.mouse.move(x(from + ((to - from) * i) / steps), y);
          if (i % 4 === 0) playSamples.push(await samplePlay(comboT0));
        }
        const beforeUp = await player.readStatus();
        const resumeStart = Date.now();
        await page.mouse.up();

        const settled = await waitExactSettle(player, 45_000);
        const err = await readError();
        settleSamples.push({
          zone: "combo",
          fraction: to,
          settleMs: settled.settleMs,
          soft: false,
          playhead: settled.status.playhead,
          engine: settled.status.engine,
          error: err,
        });
        if (settled.settleMs == null) {
          faults.push(`combo settle @ ${to}`);
        }
        if (err) faults.push(`combo scrub @ ${to}: ${err}`);

        let resumed: number | null = null;
        const ph0 = settled.status.playhead ?? beforeUp.playhead ?? 0;
        while (Date.now() - resumeStart < 20_000) {
          const s = await player.readStatus();
          playSamples.push(await samplePlay(comboT0));
          if (
            (s.playing && s.playhead != null && s.playhead > ph0) ||
            (s.playhead != null && s.playhead > ph0 + 2)
          ) {
            resumed = Date.now() - resumeStart;
            break;
          }
          if (!s.playing && Date.now() - resumeStart > 2_000) {
            await player.play.click();
          }
          await page.waitForTimeout(100);
        }
        if (resumed == null) {
          faults.push(`Play did not resume after scrub release (ph0=${ph0})`);
        } else {
          resumeMsList.push(resumed);
        }
        return resumed;
      };

      await runScrubReleaseResume(0.2, 0.55, 16);
      await page.waitForTimeout(600);
      await runScrubReleaseResume(0.55, 0.3, 12);

      for (let i = 0; i < 10; i++) {
        playSamples.push(await samplePlay(comboT0));
        await page.waitForTimeout(150);
      }
      await noteError("combo end");
    }
  }

  void opened;

  const settleByZone = (zone: ScrubZone | "combo") =>
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
      cold: settleByZone("cold"),
      gray: settleByZone("gray"),
      covered: settleByZone("covered"),
      exact: settleByZone("exact"),
      combo: settleByZone("combo"),
      allExact: summarizeMs(
        settleSamples.filter((s) => !s.soft).map((s) => s.settleMs),
      ),
    },
    resumeMs: summarizeMs(resumeMsList),
    behindMs: summarizeMs(playSamples.map((s) => s.behindMs)),
    holdMs: summarizeMs(playSamples.map((s) => s.holdMs)),
    skippedMax: playSamples.reduce((m, s) => Math.max(m, s.skipped), 0),
    holdsMax: playSamples.reduce((m, s) => Math.max(m, s.holds), 0),
    samples: {
      settle: settleSamples.length,
      play: playSamples.length,
    },
    faults,
    warnings,
    strict,
  };

  console.log(JSON.stringify(report, null, 2));

  expect(
    report.resumeMs.n,
    `no successful Play resume: ${faults.join(" | ")}`,
  ).toBeGreaterThan(0);

  // Zone decode/settle faults are the stress signal. Soft-collect by default so
  // the latency report always lands; fail the run when STRICT or FAIL_ZONES=1.
  const failZones =
    strict || process.env.WEBMEDIA_STRESS_FAIL_ZONES === "1";
  if (failZones) {
    expect(faults, faults.join("\n")).toEqual([]);
  } else if (faults.length > 0) {
    warnings.push(`zone/decode faults (non-strict): ${faults.join(" | ")}`);
    console.warn(
      "stress zone faults (set WEBMEDIA_STRESS_FAIL_ZONES=1 to fail):\n",
      faults,
    );
  }

  // Always hard-fail permanent combo death (already in faults when reopen fails).
  const fatal = faults.filter(
    (f) =>
      f.includes("Play still dead") ||
      f.includes("reopen still errored") ||
      f.startsWith("pageerror:"),
  );
  expect(fatal, fatal.join("\n")).toEqual([]);

  if (strict) {
    expect(report.settle.allExact.p95).toBeLessThan(20_000);
    if (report.behindMs.p95 != null) {
      expect(report.behindMs.p95).toBeLessThan(750);
    }
    expect(report.resumeMs.p95).toBeLessThan(8_000);
  }

  await test.info().attach("stress-scrub-av-report", {
    body: JSON.stringify({ ...report, warnings, failZones }, null, 2),
    contentType: "application/json",
  });
});
