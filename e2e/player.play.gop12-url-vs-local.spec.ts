/**
 * Compare URL vs local-file play on the same gop12 plate.
 * If local is smooth and URL freezes → cache/IO thrash; if both freeze → decode.
 */
import { expect, test } from "@playwright/test";
import fs from "node:fs";
import path from "node:path";
import { DemoPlayer } from "./helpers/player";
import { TestvideoClip, testvideoUrl } from "./helpers/testvideo";

const CLIP_URL = testvideoUrl(TestvideoClip.gop12);
const CLIP_LOCAL = path.resolve("e2e/.cache/avc_1080p_24_gop12_bf0.mp4");

function parseHolds(pipeline: string): { count: number; ms: number } {
  const m = pipeline.match(/holds\s+(\d+)\/([\d.]+)ms/);
  if (!m) return { count: 0, ms: 0 };
  return { count: Number(m[1]), ms: Number(m[2]) };
}

async function playMidSegment(
  player: DemoPlayer,
  page: import("@playwright/test").Page,
) {
  const opened = await player.readStatus();
  const last = opened.lastFrame ?? 0;
  const startAt = Math.floor(last * 0.1);
  const endAt = Math.floor(last * 0.4);
  await player.seekToFrame(startAt);
  await player.play.click();
  await expect
    .poll(async () => (await player.readStatus()).playing, { timeout: 15_000 })
    .toBe(true);

  type Sample = { t: number; playhead: number | null; seconds: number | null; pipeline: string };
  const samples: Sample[] = [];
  const deadline = Date.now() + 60_000;
  while (Date.now() < deadline) {
    const st = await player.readStatus();
    samples.push({
      t: Date.now(),
      playhead: st.playhead,
      seconds: st.seconds,
      pipeline: (await player.pipeline.innerText()).trim(),
    });
    if ((st.playhead ?? 0) >= endAt) break;
    await page.waitForTimeout(100);
  }

  const mid = samples.filter(
    (s) => (s.playhead ?? 0) > startAt + 24 && (s.playhead ?? 0) < endAt,
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
  return {
    startAt,
    endAt,
    endPlayhead: samples[samples.length - 1]?.playhead ?? null,
    realtime,
    holds: parseHolds(lastPipe),
    lastPipeline: lastPipe,
  };
}

test.describe("gop12 play URL vs local", () => {
  test("URL progressive", async ({ page }) => {
    test.setTimeout(180_000);
    const player = new DemoPlayer(page);
    await player.goto();
    await player.openUrl(CLIP_URL, 180_000);
    const report = await playMidSegment(player, page);
    console.log("URL_PLAY\n" + JSON.stringify(report, null, 2));
    await test.info().attach("url-play", {
      body: JSON.stringify(report, null, 2),
      contentType: "application/json",
    });
    expect(report.holds.ms).toBeLessThan(800);
    if (report.realtime != null) expect(report.realtime).toBeGreaterThan(0.85);
  });

  test("local file (full in BlobSource)", async ({ page }) => {
    test.setTimeout(180_000);
    test.skip(!fs.existsSync(CLIP_LOCAL), `missing ${CLIP_LOCAL}`);
    const player = new DemoPlayer(page);
    await player.goto();
    await player.openFixture(CLIP_LOCAL, 120_000);
    const report = await playMidSegment(player, page);
    console.log("LOCAL_PLAY\n" + JSON.stringify(report, null, 2));
    await test.info().attach("local-play", {
      body: JSON.stringify(report, null, 2),
      contentType: "application/json",
    });
    expect(report.holds.ms).toBeLessThan(800);
    if (report.realtime != null) expect(report.realtime).toBeGreaterThan(0.85);
  });
});
