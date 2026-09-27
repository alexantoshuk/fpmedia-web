/**
 * First PlayPrime on gray/cold without Exact-seeded presentation.
 *
 * Regression: unprimed Play awaited `warm_sample_bytes(24)` (1–3s+ lag).
 * Fix: await one GOP at head (`gop_sample_lead` 8…24).
 *
 * Path: soft-cold settle (no seed) → Play → expect `play prime warm lead=` + advance.
 */
import { expect, test } from "@playwright/test";
import {
  DemoPlayer,
  pickZoneFraction,
  type PipelineJson,
} from "./helpers/player";
import { TestvideoClip, testvideoUrl } from "./helpers/testvideo";

const CLIP_URL = testvideoUrl(TestvideoClip.gop12);
const PLAY_BUDGET_MS = 2_500;

test.describe("first Play on gray/cold (no Exact seed)", () => {
  test("soft-cold settle then PlayPrime uses GOP warm lead", async ({
    page,
  }) => {
    test.setTimeout(180_000);

    const engineLogs: string[] = [];
    page.on("console", (msg) => {
      const t = msg.text();
      if (
        t.includes("webmedia: settle") ||
        t.includes("webmedia: play prime")
      ) {
        engineLogs.push(t);
      }
    });

    const player = new DemoPlayer(page);
    await player.goto();
    await player.openUrl(CLIP_URL, 90_000);
    await expect(player.error).toHaveText("");

    const lastFrame = (await player.readStatus()).lastFrame ?? 1;

    // Tiny play then pause — leave presentation; soft-cold settle clears it.
    await player.play.click();
    await page.waitForTimeout(300);
    if ((await player.readStatus()).playing) {
      await player.play.click();
      await page.waitForTimeout(100);
    }

    let pipe: PipelineJson = await player.readPipelineJson();
    let aim =
      pickZoneFraction(pipe, lastFrame, "cold", {
        prefer: "edge",
        samples: 48,
      }) ??
      pickZoneFraction(pipe, lastFrame, "gray", {
        prefer: "edge",
        samples: 48,
      }) ??
      0.85;

    await player.scrubToFraction(aim);
    const t0 = Date.now();
    while (Date.now() - t0 < 20_000) {
      const s = await player.readStatus();
      if (
        s.playhead != null &&
        s.engine != null &&
        s.playhead === s.engine
      ) {
        break;
      }
      await page.waitForTimeout(50);
    }

    // Prefer a settle that did not Exact-seed (soft-cold). If Exact seeded,
    // pick farther and scrub again once.
    const seeded = engineLogs.some((l) =>
      l.includes("settle Exact") && l.includes("seed="),
    );
    if (seeded) {
      aim = Math.min(0.95, aim + 0.08);
      engineLogs.length = 0;
      await player.scrubToFraction(aim);
      const t1 = Date.now();
      while (Date.now() - t1 < 20_000) {
        const s = await player.readStatus();
        if (
          s.playhead != null &&
          s.engine != null &&
          s.playhead === s.engine
        ) {
          break;
        }
        await page.waitForTimeout(50);
      }
    }

    const ph0 = (await player.readStatus()).playhead ?? 0;
    const playStart = Date.now();
    await player.play.click();

    await expect
      .poll(async () => (await player.readStatus()).playing, {
        timeout: PLAY_BUDGET_MS,
        message: `Play after soft settle did not start within ${PLAY_BUDGET_MS}ms`,
      })
      .toBe(true);

    await expect
      .poll(async () => (await player.readStatus()).playhead ?? 0, {
        timeout: PLAY_BUDGET_MS,
        message: "playhead did not advance after gray/cold→Play",
      })
      .toBeGreaterThan(ph0);

    const resumeMs = Date.now() - playStart;
    expect(
      resumeMs,
      `Play resume ${resumeMs}ms (budget ${PLAY_BUDGET_MS}); logs=${JSON.stringify(engineLogs.slice(-12))}`,
    ).toBeLessThan(PLAY_BUDGET_MS);

    expect(await player.error.innerText()).toBe("");

    const warmLead = engineLogs.find((l) =>
      l.includes("play prime warm lead="),
    );
    const skipFill = engineLogs.some((l) =>
      l.includes("play prime skip fill"),
    );
    // Unprimed path: GOP warm lead. If settle seeded Exact, skip-fill is OK.
    if (!skipFill) {
      expect(
        warmLead,
        `expected play prime warm lead=…; got ${JSON.stringify(engineLogs.slice(-16))}`,
      ).toBeTruthy();
      const m = warmLead!.match(/warm lead=(\d+)/);
      const lead = m ? Number(m[1]) : -1;
      expect(
        lead,
        `warm lead ${lead} should be GOP-capped (8…24)`,
      ).toBeGreaterThanOrEqual(8);
      expect(lead).toBeLessThanOrEqual(24);
    }
  });
});
