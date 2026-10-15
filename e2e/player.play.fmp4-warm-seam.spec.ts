/**
 * fMP4 warm-open first-play seam: play from frame 0 past ~120 without waiting
 * for full mfra backfill. Catches index / cold-mdat hitches (~108–119) that
 * mid-file gop12 probes miss.
 *
 * Clip: testvideo `avc_1080p_24_fmp4_gop12_bf0`.
 */
import { expect, test } from "@playwright/test";
import { DemoPlayer } from "./helpers/player";
import { TestvideoClip, testvideoUrl } from "./helpers/testvideo";

const CLIP_URL = testvideoUrl(TestvideoClip.fmp4);
/** End past the reported A/V hitch (~130) and warm window (12×GOP12=144). */
const END_FRAME = 200;
const SEAM_LO = 100;
const SEAM_HI = 160;

function parseHolds(pipeline: string): { count: number; ms: number } {
  const m = pipeline.match(/holds\s+(\d+)\/([\d.]+)ms/);
  if (!m) return { count: 0, ms: 0 };
  return { count: Number(m[1]), ms: Number(m[2]) };
}

function parseLagMs(pipeline: string): number | null {
  const m = pipeline.match(/lag\s+([\d.]+)ms/);
  return m ? Number(m[1]) : null;
}

test.describe("fMP4 warm-seam first play (Chrome)", () => {
  test("frame 0 → 200: no long underrun near warm/chunk seam", async ({
    page,
  }) => {
    test.setTimeout(240_000);

    const consoleLines: string[] = [];
    const faults: string[] = [];
    page.on("console", (msg) => {
      const t = msg.text();
      if (
        t.includes("fragment backfill") ||
        t.includes("fpmedia-web:")
      ) {
        consoleLines.push(t);
      }
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
    // Ready = warm open. Play immediately (do **not** wait for full lastFrame) —
    // prime must build index lead before the clock.
    await player.openUrl(CLIP_URL, 300_000);
    await expect(player.error).toHaveText("");

    const opened = await player.readStatus();
    // Already at 0 after open — avoid seek (can fight warm backfill on fMP4).
    if ((opened.playhead ?? 0) !== 0) {
      await player.seekToFrame(0);
    }

    await player.play.click({ timeout: 60_000 });
    await expect
      .poll(async () => (await player.readStatus()).playing, {
        timeout: 60_000,
      })
      .toBe(true);

    type Sample = {
      t: number;
      playhead: number | null;
      seconds: number | null;
      pipeline: string;
      lastFrame: number | null;
    };
    const samples: Sample[] = [];
    let stallMs = 0;
    let maxStallMs = 0;
    let prevHead: number | null = null;
    let prevT = Date.now();
    let stallAt: number | null = null;

    const deadline = Date.now() + 90_000;
    while (Date.now() < deadline) {
      const st = await player.readStatus();
      const now = Date.now();
      const pipeline = (await player.pipeline.innerText()).trim();
      samples.push({
        t: now,
        playhead: st.playhead,
        seconds: st.seconds,
        pipeline,
        lastFrame: st.lastFrame,
      });

      const head = st.playhead ?? 0;
      if (prevHead != null && head === prevHead && st.playing) {
        const dt = now - prevT;
        stallMs += dt;
        if (dt > maxStallMs) {
          maxStallMs = dt;
          stallAt = head;
        }
      } else {
        stallMs = 0;
      }
      prevHead = head;
      prevT = now;

      if (head >= END_FRAME) break;
      expect(st.playing, `stopped early @ ${head}`).toBe(true);
      await page.waitForTimeout(50);
    }

    const endPlayhead = samples[samples.length - 1]?.playhead ?? 0;
    const seam = samples.filter((s) => {
      const h = s.playhead ?? 0;
      return h >= SEAM_LO && h <= SEAM_HI;
    });
    const early = samples.filter((s) => (s.playhead ?? 0) < SEAM_LO);

    function windowRealtime(
      win: Sample[],
    ): { realtime: number | null; maxLagMs: number; holds: { count: number; ms: number } } {
      let realtime: number | null = null;
      if (win.length >= 2) {
        const a = win[0]!;
        const b = win[win.length - 1]!;
        const dWall = (b.t - a.t) / 1000;
        const dMedia = (b.seconds ?? 0) - (a.seconds ?? 0);
        if (dWall > 0.3 && dMedia > 0.2) realtime = dMedia / dWall;
      }
      const maxLagMs = win
        .map((s) => parseLagMs(s.pipeline))
        .filter((v): v is number => v != null)
        .reduce((m, v) => Math.max(m, v), 0);
      const holds = parseHolds(win[win.length - 1]?.pipeline ?? "");
      return { realtime, maxLagMs, holds };
    }

    const earlyStats = windowRealtime(early);
    const seamStats = windowRealtime(seam);
    const lastPipe = samples[samples.length - 1]?.pipeline ?? "";
    const totalHolds = parseHolds(lastPipe);

    // Largest single playhead freeze (consecutive equal samples while playing).
    let worstFreezeMs = 0;
    let worstFreezeAt: number | null = null;
    for (let i = 1; i < samples.length; i++) {
      const a = samples[i - 1]!;
      const b = samples[i]!;
      if (a.playhead != null && a.playhead === b.playhead) {
        const dt = b.t - a.t;
        // accumulate run
        let j = i;
        let end = b.t;
        while (
          j + 1 < samples.length &&
          samples[j + 1]!.playhead === a.playhead
        ) {
          j++;
          end = samples[j]!.t;
        }
        const freeze = end - a.t;
        if (freeze > worstFreezeMs) {
          worstFreezeMs = freeze;
          worstFreezeAt = a.playhead;
        }
        i = j;
      }
    }

    const report = {
      clip: TestvideoClip.fmp4,
      openedLastFrame: opened.lastFrame,
      endPlayhead,
      early: earlyStats,
      seam: seamStats,
      totalHolds,
      worstFreezeMs,
      worstFreezeAt,
      maxSampleGapStallMs: maxStallMs,
      stallAt,
      lastPipeline: lastPipe,
      backfillLogs: consoleLines.filter((l) => l.includes("fragment backfill")).slice(-30),
      faults: faults.slice(-20),
    };
    console.log("FMP4_WARM_SEAM_REPORT\n" + JSON.stringify(report, null, 2));
    await test.info().attach("fmp4-warm-seam-report", {
      body: JSON.stringify(report, null, 2),
      contentType: "application/json",
    });

    expect(endPlayhead, "did not reach end frame").toBeGreaterThanOrEqual(
      END_FRAME - 5,
    );
    expect(faults.filter((f) => f.includes("flush/idle"))).toEqual([]);
    // Soft budgets: no ~1s playhead freeze (user-visible hitch at 35/108/131).
    // Warm mdat + Play pauses bg sync-verify — seam @100–160 must stay near realtime.
    expect(earlyStats.holds.ms, "early underrun holds").toBeLessThan(400);
    if (earlyStats.realtime != null) {
      expect(earlyStats.realtime, `early realtime ${earlyStats.realtime}`).toBeGreaterThan(
        0.85,
      );
    }
    expect(seamStats.holds.ms, "seam underrun holds @~130").toBeLessThan(400);
    if (seamStats.realtime != null) {
      expect(
        seamStats.realtime,
        `seam realtime ${seamStats.realtime} (user hitch ~130)`,
      ).toBeGreaterThan(0.85);
    }
    expect(
      worstFreezeMs,
      `playhead freeze ${worstFreezeMs}ms @ frame ${worstFreezeAt}`,
    ).toBeLessThan(500);
    expect(
      totalHolds.ms,
      `underrun hold ${totalHolds.count}/${totalHolds.ms}ms`,
    ).toBeLessThan(800);
  });
});
