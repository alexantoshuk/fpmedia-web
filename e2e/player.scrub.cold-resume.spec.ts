/**
 * Cold (non-gray) scrub settle → Play must resume quickly.
 *
 * Regression: soft-cold left an empty presentation queue; PlayPrime then awaited
 * `warm_sample_bytes(24)` on a cold URL hole (7–16s). Fix: settle awaits one GOP
 * warm → Exact+seed so PlayPrime skips fill.
 *
 * Clip: testvideo progressive `avc_1080p_24_gop12_bf0` via `/proxy`.
 */
import { expect, test } from "@playwright/test";
import {
  DemoPlayer,
  pickZoneFraction,
  type PipelineJson,
} from "./helpers/player";
import { TestvideoClip, testvideoUrl } from "./helpers/testvideo";

const CLIP_URL = testvideoUrl(TestvideoClip.gop12);

/** Click→Play advancing within this budget (cold GOP warm + Exact + prime). */
const COLD_RESUME_MS = 4_000;

async function waitEngMatchesPlayhead(
  player: DemoPlayer,
  timeoutMs: number,
): Promise<number> {
  const t0 = Date.now();
  while (Date.now() - t0 < timeoutMs) {
    const s = await player.readStatus();
    if (
      s.playhead != null &&
      s.engine != null &&
      s.playhead === s.engine
    ) {
      return Date.now() - t0;
    }
    await player.page.waitForTimeout(50);
  }
  throw new Error(`eng≠playhead after ${timeoutMs}ms`);
}

test.describe("cold scrub → Play resume", () => {
  test("progressive URL: cold tap settle then Play under budget", async ({
    page,
  }) => {
    test.setTimeout(180_000);

    const engineLogs: string[] = [];
    page.on("console", (msg) => {
      const t = msg.text();
      if (
        t.includes("fpmedia-web: settle") ||
        t.includes("fpmedia-web: play prime")
      ) {
        engineLogs.push(t);
      }
    });

    const player = new DemoPlayer(page);
    await player.goto();
    await player.openUrl(CLIP_URL, 90_000);
    await expect(player.error).toHaveText("");

    const lastFrame = (await player.readStatus()).lastFrame ?? 1;

    // Tiny play so open is past Ready; do not wait for full gray.
    await player.play.click();
    await page.waitForTimeout(400);
    if ((await player.readStatus()).playing) {
      await player.play.click();
      await page.waitForTimeout(100);
    }

    let pipe: PipelineJson = await player.readPipelineJson();
    let coldFrac = pickZoneFraction(pipe, lastFrame, "cold", {
      prefer: "edge",
      samples: 48,
    });
    // Gray may already cover early zones — aim far if no cold sample.
    if (coldFrac == null) {
      coldFrac = 0.88;
    }

    await player.scrubToFraction(coldFrac);
    const settleMs = await waitEngMatchesPlayhead(player, 20_000);
    expect(settleMs, `cold settle too slow: ${settleMs}ms`).toBeLessThan(
      COLD_RESUME_MS + 2_000,
    );

    const ph0 = (await player.readStatus()).playhead ?? 0;
    const playStart = Date.now();
    await player.play.click();

    await expect
      .poll(async () => (await player.readStatus()).playing, {
        timeout: COLD_RESUME_MS,
        message: `Play after cold scrub did not start within ${COLD_RESUME_MS}ms`,
      })
      .toBe(true);

    await expect
      .poll(async () => (await player.readStatus()).playhead ?? 0, {
        timeout: COLD_RESUME_MS,
        message: "playhead did not advance after cold scrub→Play",
      })
      .toBeGreaterThan(ph0);

    const resumeMs = Date.now() - playStart;
    expect(
      resumeMs,
      `cold scrub→Play resume ${resumeMs}ms (budget ${COLD_RESUME_MS}); logs=${JSON.stringify(engineLogs.slice(-12))}`,
    ).toBeLessThan(COLD_RESUME_MS);

    expect(await player.error.innerText()).toBe("");

    const sawGopWarmOrSkip =
      engineLogs.some((l) => l.includes("gop_warm=") || l.includes("skip fill")) ||
      engineLogs.some((l) => l.includes("play prime warm lead="));
    expect(
      sawGopWarmOrSkip,
      `expected gop-warm settle or skip-fill / capped prime warm; got ${JSON.stringify(engineLogs.slice(-16))}`,
    ).toBe(true);
  });
});
