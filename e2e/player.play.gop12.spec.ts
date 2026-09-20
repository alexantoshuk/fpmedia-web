/**
 * Play stability probe: testvideo AVC 1080p 24fps GOP12 bf0 (full mid-play).
 * Temporary diagnostic — gray fetch strip ≠ decode buffer.
 */
import { expect, test } from "@playwright/test";
import { DemoPlayer } from "./helpers/player";
import { TestvideoClip, testvideoUrl } from "./helpers/testvideo";

const CLIP_URL = testvideoUrl(TestvideoClip.gop12);

function parseHolds(pipeline: string): { count: number; ms: number } {
  const m = pipeline.match(/holds\s+(\d+)\/([\d.]+)ms/);
  if (!m) return { count: 0, ms: 0 };
  return { count: Number(m[1]), ms: Number(m[2]) };
}

function parseLagMs(pipeline: string): number | null {
  const m = pipeline.match(/lag\s+([\d.]+)ms/);
  return m ? Number(m[1]) : null;
}

test.describe("gop12 play (Chrome)", () => {
  test("mid-play realtime and underrun holds", async ({ page }) => {
    test.setTimeout(180_000);

    const faults: string[] = [];
    page.on("console", (msg) => {
      const t = msg.text();
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
    await player.openUrl(CLIP_URL, 180_000);
    await expect(player.error).toHaveText("");

    const opened = await player.readStatus();
    const last = opened.lastFrame ?? 0;
    expect(last).toBeGreaterThan(1000);

    // Start a bit in so open densify / verify noise settles.
    const startAt = Math.floor(last * 0.1);
    const endAt = Math.floor(last * 0.55);
    await player.seekToFrame(startAt);

    await player.play.click();
    await expect
      .poll(async () => (await player.readStatus()).playing, { timeout: 15_000 })
      .toBe(true);

    type Sample = {
      t: number;
      playhead: number | null;
      seconds: number | null;
      pipeline: string;
    };
    const samples: Sample[] = [];
    const deadline = Date.now() + 90_000;
    while (Date.now() < deadline) {
      const st = await player.readStatus();
      const pipeline = (await player.pipeline.innerText()).trim();
      samples.push({
        t: Date.now(),
        playhead: st.playhead,
        seconds: st.seconds,
        pipeline,
      });
      if ((st.playhead ?? 0) >= endAt) break;
      expect(st.playing, `stopped early @ ${st.playhead}`).toBe(true);
      await page.waitForTimeout(100);
    }

    const mid = samples.filter(
      (s) =>
        (s.playhead ?? 0) > startAt + 24 &&
        (s.playhead ?? 0) < endAt,
    );
    let realtime: number | null = null;
    if (mid.length >= 2) {
      const a = mid[0]!;
      const b = mid[mid.length - 1]!;
      const dWall = (b.t - a.t) / 1000;
      const dMedia = (b.seconds ?? 0) - (a.seconds ?? 0);
      if (dWall > 1 && dMedia > 1) realtime = dMedia / dWall;
    }

    const lastPipe = samples[samples.length - 1]?.pipeline ?? "";
    const holds = parseHolds(lastPipe);
    const maxLag = samples
      .map((s) => parseLagMs(s.pipeline))
      .filter((v): v is number => v != null)
      .reduce((m, v) => Math.max(m, v), 0);

    const fetched = await page.getByTestId("scrubber-fetched").getAttribute("style");
    const report = {
      clip: TestvideoClip.gop12,
      startAt,
      endAt,
      endPlayhead: samples[samples.length - 1]?.playhead,
      realtime,
      holds,
      maxLagMs: maxLag,
      lastPipeline: lastPipe,
      fetchedStyleSnippet: fetched?.slice(0, 200) ?? null,
      faults: faults.slice(-20),
    };
    console.log("GOP12_PLAY_REPORT\n" + JSON.stringify(report, null, 2));
    await test.info().attach("gop12-play-report", {
      body: JSON.stringify(report, null, 2),
      contentType: "application/json",
    });

    expect(faults.filter((f) => f.includes("flush/idle"))).toEqual([]);
    expect(
      holds.ms,
      `underrun hold too high: ${holds.count}/${holds.ms}ms`,
    ).toBeLessThan(800);
    if (realtime != null) {
      expect(realtime, `realtime ${realtime}`).toBeGreaterThan(0.85);
    }
  });
});
