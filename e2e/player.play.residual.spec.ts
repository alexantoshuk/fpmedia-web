/**
 * Play residual probe — paint fps + held/presentation + skip/clock correlation.
 *
 *   WEBMEDIA_CLIP=gop12|gop48|/path.mov  npx playwright test e2e/player.play.residual.spec.ts
 *   WEBMEDIA_RESIDUAL_MUTE=1             mute before play (AAC still drives clock)
 *
 * Soft health asserts only. mediabunny: updateNextFrame overdue skip-to-latest.
 */
import { expect, test } from "@playwright/test";
import { DemoPlayer, type PipelineJson } from "./helpers/player";

const SAMPLE_MS = 50;
const WINDOW_MS = 1000;
/** Longer window dilutes demo UI sync throttle (~100 ms) on playhead/status. */
const WINDOW_MS_LONG = 5000;
/** Windows at or below this fps (relative to clip) are “low” for correlation. */
const LOW_FPS_ABS = 23.5;

type Sample = {
  t: number;
  playhead: number | null;
  /** Engine frame when status shows `(eng N)`; else same as playhead. */
  engine: number | null;
  seconds: number | null;
  /** Demo fps meter from status (`· 24.5 fps`), if present. */
  uiFps: number | null;
  held_len: number | null;
  presentation_len: number;
  frames_skipped: number;
  underrun_holds: number;
  underrun_hold_ms: number;
  last_behind_ms: number | null;
  decode_queue_size: number | null;
};

type FpsWindow = {
  t0: number;
  t1: number;
  playhead0: number;
  playhead1: number;
  fps: number;
  skipDelta: number;
  maxBehindMs: number;
  heldMin: number | null;
  presMin: number;
  stallMs: number;
};

function finiteHeld(v: number | null): number | null {
  if (v == null || !Number.isFinite(v) || v === Number.MAX_SAFE_INTEGER) {
    return null;
  }
  if (v > 1_000_000) return null;
  return v;
}

function percentile(sorted: number[], p: number): number | null {
  if (sorted.length === 0) return null;
  const idx = Math.min(
    sorted.length - 1,
    Math.max(0, Math.ceil(p * sorted.length) - 1),
  );
  return sorted[idx]!;
}

function frameOf(s: Sample): number {
  return s.engine ?? s.playhead ?? 0;
}

function fpsWindows(samples: Sample[], windowMs: number): FpsWindow[] {
  const out: FpsWindow[] = [];
  let j = 0;
  for (let i = 0; i < samples.length; i++) {
    const a = samples[i]!;
    while (j + 1 < samples.length && samples[j + 1]!.t - a.t < windowMs) {
      j++;
    }
    const b = samples[j]!;
    const dWall = (b.t - a.t) / 1000;
    const dFrames = frameOf(b) - frameOf(a);
    if (dWall < windowMs / 1000 / 2 || dFrames < 0) continue;

    const slice = samples.slice(i, j + 1);
    let stallMs = 0;
    for (let k = 1; k < slice.length; k++) {
      const prev = slice[k - 1]!;
      const cur = slice[k]!;
      if (frameOf(cur) <= frameOf(prev)) {
        stallMs += cur.t - prev.t;
      }
    }
    const heldVals = slice
      .map((s) => finiteHeld(s.held_len))
      .filter((v): v is number => v != null);
    out.push({
      t0: a.t,
      t1: b.t,
      playhead0: frameOf(a),
      playhead1: frameOf(b),
      fps: dFrames / dWall,
      skipDelta: b.frames_skipped - a.frames_skipped,
      maxBehindMs: slice
        .map((s) => s.last_behind_ms)
        .filter((v): v is number => v != null)
        .reduce((m, v) => Math.max(m, v), 0),
      heldMin: heldVals.length ? Math.min(...heldVals) : null,
      presMin: Math.min(...slice.map((s) => s.presentation_len)),
      stallMs,
    });
  }
  return out;
}

function fpsStats(windows: FpsWindow[]) {
  const fpsSeries = windows.map((w) => w.fps).sort((x, y) => x - y);
  return {
    n: fpsSeries.length,
    min: fpsSeries[0] ?? null,
    p05: percentile(fpsSeries, 0.05),
    p50: percentile(fpsSeries, 0.5),
    mean:
      fpsSeries.length > 0
        ? fpsSeries.reduce((a, b) => a + b, 0) / fpsSeries.length
        : null,
  };
}

function summarize(samples: Sample[], startAt: number, endAt: number) {
  const mid = samples.filter(
    (s) =>
      (s.playhead ?? 0) > startAt + 24 && (s.playhead ?? 0) < endAt,
  );

  let realtime: number | null = null;
  if (mid.length >= 2) {
    const a = mid[0]!;
    const b = mid[mid.length - 1]!;
    const dWall = (b.t - a.t) / 1000;
    const dMedia = (b.seconds ?? 0) - (a.seconds ?? 0);
    if (dWall > 1 && dMedia > 1) realtime = dMedia / dWall;
  }

  const windows1s = fpsWindows(mid, WINDOW_MS);
  const windows5s = fpsWindows(mid, WINDOW_MS_LONG);
  const heldSeries = mid
    .map((s) => finiteHeld(s.held_len))
    .filter((v): v is number => v != null);
  const presSeries = mid.map((s) => s.presentation_len);
  const heldSorted = [...heldSeries].sort((x, y) => x - y);
  const presSorted = [...presSeries].sort((x, y) => x - y);
  const uiFpsSeries = mid
    .map((s) => s.uiFps)
    .filter((v): v is number => v != null)
    .sort((x, y) => x - y);

  const low = windows1s.filter((w) => w.fps <= LOW_FPS_ABS);
  const lowByFps = [...low].sort((a, b) => a.fps - b.fps).slice(0, 8);

  const last = mid[mid.length - 1] ?? samples[samples.length - 1];
  return {
    sampleCount: samples.length,
    midCount: mid.length,
    realtime,
    /** Demo play path syncs status ~10 Hz — 1 s Δplayhead can under-read by ~2–3 frames. */
    uiSyncNote:
      "play sync_ui throttled to ~100ms; prefer fps5s / uiFpsMeter vs fps1s.min",
    fps: { windowMs: WINDOW_MS, ...fpsStats(windows1s) },
    fps5s: { windowMs: WINDOW_MS_LONG, ...fpsStats(windows5s) },
    uiFpsMeter: {
      n: uiFpsSeries.length,
      min: uiFpsSeries[0] ?? null,
      p05: percentile(uiFpsSeries, 0.05),
      p50: percentile(uiFpsSeries, 0.5),
      mean:
        uiFpsSeries.length > 0
          ? uiFpsSeries.reduce((a, b) => a + b, 0) / uiFpsSeries.length
          : null,
    },
    held: {
      n: heldSorted.length,
      min: heldSorted[0] ?? null,
      p50: percentile(heldSorted, 0.5),
      max: heldSorted[heldSorted.length - 1] ?? null,
    },
    presentation: {
      n: presSorted.length,
      min: presSorted[0] ?? null,
      p50: percentile(presSorted, 0.5),
      max: presSorted[presSorted.length - 1] ?? null,
    },
    skips: {
      start: samples[0]?.frames_skipped ?? 0,
      end: last?.frames_skipped ?? 0,
      delta:
        (last?.frames_skipped ?? 0) - (samples[0]?.frames_skipped ?? 0),
    },
    holds: {
      count: last?.underrun_holds ?? 0,
      ms: last?.underrun_hold_ms ?? 0,
    },
    maxBehindMs: mid
      .map((s) => s.last_behind_ms)
      .filter((v): v is number => v != null)
      .reduce((m, v) => Math.max(m, v), 0),
    /** Skip / present / stall correlation in low 1 s windows. */
    lowFps: {
      threshold: LOW_FPS_ABS,
      count: low.length,
      withSkips: low.filter((w) => w.skipDelta > 0).length,
      withStall: low.filter((w) => w.stallMs > 100).length,
      meanSkipDelta:
        low.length > 0
          ? low.reduce((a, w) => a + w.skipDelta, 0) / low.length
          : null,
      meanStallMs:
        low.length > 0
          ? low.reduce((a, w) => a + w.stallMs, 0) / low.length
          : null,
      meanBehindMs:
        low.length > 0
          ? low.reduce((a, w) => a + w.maxBehindMs, 0) / low.length
          : null,
      worst: lowByFps.map((w) => ({
        fps: +w.fps.toFixed(3),
        skipDelta: w.skipDelta,
        stallMs: w.stallMs,
        maxBehindMs: +w.maxBehindMs.toFixed(3),
        heldMin: w.heldMin,
        presMin: w.presMin,
        playhead: `${w.playhead0}→${w.playhead1}`,
      })),
    },
  };
}

function classifyClock(consoleLines: string[], meta: string): {
  kind: "aac" | "silent" | "wall" | "unknown";
  evidence: string[];
  metaHasAudio: boolean;
} {
  const evidence = consoleLines.filter(
    (t) =>
      t.includes("webmedia: audio") ||
      t.includes("webmedia: PlaybackClock") ||
      t.includes("webmedia: no audio"),
  );
  const metaHasAudio = /\baudio\b/i.test(meta);
  if (evidence.some((t) => t.includes("audio track ready"))) {
    return { kind: "aac", evidence, metaHasAudio };
  }
  if (evidence.some((t) => t.includes("PlaybackClock (silent"))) {
    return { kind: "silent", evidence, metaHasAudio };
  }
  if (evidence.some((t) => t.includes("PlaybackClock disabled"))) {
    return { kind: "wall", evidence, metaHasAudio };
  }
  if (evidence.some((t) => t.includes("audio disabled") || t.includes("no audio"))) {
    return { kind: "silent", evidence, metaHasAudio };
  }
  return { kind: "unknown", evidence, metaHasAudio };
}

test.describe("play residual (fps / held / presentation)", () => {
  test("mid-play residual report", async ({ page }) => {
    test.setTimeout(180_000);

    const faults: string[] = [];
    const consoleLines: string[] = [];
    page.on("console", (msg) => {
      const t = msg.text();
      consoleLines.push(t);
      if (
        msg.type() === "error" ||
        t.includes("flush/idle") ||
        t.includes("backpressure timed out")
      ) {
        faults.push(t);
      }
    });
    page.on("pageerror", (err) => faults.push(String(err)));

    const player = new DemoPlayer(page);
    await player.goto();
    const clip = await player.openClip(process.env.WEBMEDIA_CLIP ?? "gop12");
    await expect(player.error).toHaveText("");

    const meta = (await player.meta.innerText()).trim();
    const clock = classifyClock(consoleLines, meta);

    const mute = process.env.WEBMEDIA_RESIDUAL_MUTE === "1";
    if (mute) {
      const muteBtn = page.getByTestId("btn-mute");
      if (await muteBtn.isEnabled()) {
        await muteBtn.click();
      }
    }

    const opened = await player.readStatus();
    const last = opened.lastFrame ?? 0;
    expect(last).toBeGreaterThan(100);

    const startAt = Math.floor(last * 0.1);
    const endAt = Math.floor(last * 0.45);
    await player.seekToFrame(startAt);

    await player.play.click();
    await expect
      .poll(async () => (await player.readStatus()).playing, {
        timeout: 15_000,
      })
      .toBe(true);

    const samples: Sample[] = [];
    const deadline = Date.now() + 90_000;
    while (Date.now() < deadline) {
      const st = await player.readStatus();
      const pipe: PipelineJson = await player.readPipelineJson();
      const uiFpsMatch = st.raw.match(/·\s*([\d.]+)\s*fps/i);
      samples.push({
        t: Date.now(),
        playhead: st.playhead,
        engine: st.engine,
        seconds: st.seconds,
        uiFps: uiFpsMatch ? Number(uiFpsMatch[1]) : null,
        held_len: pipe.held_len,
        presentation_len: pipe.presentation_len ?? 0,
        frames_skipped: pipe.frames_skipped,
        underrun_holds: pipe.underrun_holds,
        underrun_hold_ms: pipe.underrun_hold_ms,
        last_behind_ms: pipe.last_behind_ms,
        decode_queue_size: pipe.decode_queue_size,
      });
      if ((st.playhead ?? 0) >= endAt) break;
      expect(st.playing, `stopped early @ ${st.playhead}`).toBe(true);
      await page.waitForTimeout(SAMPLE_MS);
    }

    const stats = summarize(samples, startAt, endAt);
    const report = {
      clip,
      meta,
      clock,
      mute,
      startAt,
      endAt,
      endPlayhead: samples[samples.length - 1]?.playhead ?? null,
      ...stats,
      fps_min: stats.fps.min,
      fps5s_min: stats.fps5s.min,
      held_min: stats.held.min,
      pres_min: stats.presentation.min,
      faults: faults.slice(-20),
    };

    console.log("PLAY_RESIDUAL_REPORT\n" + JSON.stringify(report, null, 2));
    await test.info().attach("play-residual-report", {
      body: JSON.stringify(report, null, 2),
      contentType: "application/json",
    });

    expect(
      faults.filter((f) => f.includes("flush/idle")),
      `flush/idle: ${faults.filter((f) => f.includes("flush/idle")).join("; ")}`,
    ).toEqual([]);
    expect(
      stats.holds.ms,
      `underrun hold too high: ${stats.holds.count}/${stats.holds.ms}ms`,
    ).toBeLessThan(800);
    // Wall-driven silent clock + AAC should both hold ≈1× after keep-alive fix.
    if (stats.realtime != null) {
      expect(stats.realtime, `realtime ${stats.realtime}`).toBeGreaterThan(
        0.85,
      );
    }
  });
});
