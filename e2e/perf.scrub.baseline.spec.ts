/**
 * Scrub mechanic baselines for testvideo clips (save / compare).
 *
 *   WEBMEDIA_BASELINE_MODE=save    npm run e2e:baseline:save
 *   WEBMEDIA_BASELINE_MODE=compare npm run e2e:baseline:compare
 *
 * Requires app on :3000 + network (GitHub Release via Trunk `/media/`).
 * Clips: see `e2e/helpers/testvideo.ts`.
 */
import { expect, test } from "@playwright/test";
import fs from "node:fs";
import {
  BASELINE_TAG,
  PERF_CLIPS,
  baselineMode,
  baselinePath,
  compareBaselines,
  loadBaseline,
  runClipBaseline,
  saveBaseline,
  saveCompareReport,
} from "./helpers/scrub-baseline";

test.describe.configure({ timeout: 600_000 });

for (const clip of PERF_CLIPS) {
  test(`scrub baseline [${clip.id}] ${clip.label}`, async ({ page }, testInfo) => {
    const mode = baselineMode();
    const report = await runClipBaseline(page, clip);

    await testInfo.attach(`baseline-${clip.id}-run`, {
      body: JSON.stringify(report, null, 2),
      contentType: "application/json",
    });

    console.log(
      JSON.stringify(
        {
          mode,
          tag: BASELINE_TAG,
          clip: clip.id,
          openMs: report.openMs,
          lastFrame: report.afterOpen.lastFrame,
          scenarios: report.scenarios.map((s) => ({
            id: s.id,
            dragMs: s.dragMs,
            settleMs: s.settleMs,
            exactOk: s.exactOk,
            timedOut: s.settleTimedOut,
            end: `${s.endPlayhead}/${s.endEngine}`,
          })),
        },
        null,
        2,
      ),
    );

    if (mode === "save") {
      const out = await saveBaseline(report);
      console.log(`saved baseline → ${out}`);
      expect(fs.existsSync(out)).toBe(true);
      const failed = report.scenarios.filter((s) => !s.exactOk);
      if (failed.length) {
        console.warn(
          `save: ${failed.length} scenario(s) did not reach exact:`,
          failed.map((s) => `${s.id} settle=${s.settleMs}ms ${s.endStatus}`),
        );
      }
      return;
    }

    // Require exact only where the saved baseline painted exact.
    const baselineFile = baselinePath(clip.id);
    expect(
      fs.existsSync(baselineFile),
      `no baseline at ${baselineFile} — run e2e:baseline:save first`,
    ).toBe(true);

    const baseline = await loadBaseline(clip.id);
    for (const s of report.scenarios) {
      const b = baseline.scenarios.find((x) => x.id === s.id);
      if (b?.exactOk) {
        expect(
          s.exactOk,
          `${clip.id}/${s.id}: exact not painted — ${s.endStatus}`,
        ).toBe(true);
        expect(s.settleTimedOut, `${clip.id}/${s.id}: settle timed out`).toBe(
          false,
        );
      }
    }

    const findings = compareBaselines(baseline, report);
    const compareOut = await saveCompareReport(
      clip.id,
      baseline,
      report,
      findings,
    );
    console.log(`compare report → ${compareOut}`);
    await testInfo.attach(`baseline-${clip.id}-findings`, {
      body: JSON.stringify(findings, null, 2),
      contentType: "application/json",
    });

    expect(
      findings,
      findings.map((f) => `${f.scenarioId}.${f.metric}: ${f.detail}`).join("\n"),
    ).toEqual([]);
  });
}
