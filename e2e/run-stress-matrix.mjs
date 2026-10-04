/**
 * Run e2e:stress one clip at a time. After each clip, write that clip's JSON
 * (via WEBMEDIA_STRESS_REPORT_PATH) and refresh summary.json / summary.md.
 *
 * Usage (app on :3000):
 *   npm run e2e:stress:matrix          # AVC (default)
 *   npm run e2e:stress:matrix:hevc     # HEVC (needs WASM built with hevc)
 *
 * Env:
 *   WEBMEDIA_STRESS_CODEC  avc (default) | hevc  (or argv: `node … hevc`)
 *   WEBMEDIA_STRESS_OUT    default e2e/stress-reports or …-hevc
 *   TESTVIDEO_BASE         default …/download/v0.3.0
 *   WEBMEDIA_STRESS_FORCE=1  re-run clips that already have a .json
 *
 * Note: do not write under test-results/ — Playwright wipes that dir each run.
 */
import { spawn } from "node:child_process";
import fs from "node:fs/promises";
import path from "node:path";

/** testvideo v0.3.0 AAC mp4 matrix (no HEVC, no PCM .mov). */
const AVC_CLIPS = [
  "avc_1080p_25_fmp4_gop12_bf0",
  "avc_1080p_25_gop12_bf0",
  "avc_1080p_25_gop12_bf2",
  "avc_1080p_25_gop33_bf2",
  "avc_1080p_25_gop48_bf0",
  "avc_1080p_25_scenecut_bf0",
  "avc_1080p_2997_fmp4_gop12_bf0",
  "avc_1080p_2997_gop12_bf0",
  "avc_1080p_2997_gop12_bf2",
  "avc_1080p_2997_gop33_bf2",
  "avc_1080p_2997_gop48_bf0",
  "avc_1080p_2997_scenecut_bf0",
  "avc_1080p_60_fmp4_gop12_bf0",
  "avc_1080p_60_gop12_bf0",
  "avc_1080p_60_gop12_bf2",
  "avc_1080p_60_gop33_bf2",
  "avc_1080p_60_gop48_bf0",
  "avc_1080p_60_scenecut_bf0",
];

const HEVC_CLIPS = [
  "hevc_1080p_25_gop12_bf0",
  "hevc_1080p_2997_gop12_bf0",
  "hevc_1080p_60_gop12_bf0",
];

const codecArg = process.argv
  .slice(2)
  .find((a) => a === "avc" || a === "hevc");
const CODEC = (
  codecArg ??
  process.env.WEBMEDIA_STRESS_CODEC ??
  "avc"
).toLowerCase();
if (CODEC !== "avc" && CODEC !== "hevc") {
  console.error(`codec must be avc|hevc (got ${CODEC})`);
  process.exit(2);
}

const CLIPS = CODEC === "hevc" ? HEVC_CLIPS : AVC_CLIPS;
const OUT =
  process.env.WEBMEDIA_STRESS_OUT ??
  (CODEC === "hevc" ? "e2e/stress-reports-hevc" : "e2e/stress-reports");
const BASE = (
  process.env.TESTVIDEO_BASE ??
  "https://github.com/alexantoshuk/testvideo/releases/download/v0.3.0"
).replace(/\/$/, "");
const FORCE = process.env.WEBMEDIA_STRESS_FORCE === "1";

function rowFrom(clip, report, exitCode, elapsedMs) {
  return {
    clip,
    ok: exitCode === 0,
    exitCode,
    elapsedMs,
    openMs: report?.openMs ?? null,
    settleColdP95: report?.settle?.cold?.p95 ?? null,
    settleGrayP95: report?.settle?.gray?.p95 ?? null,
    settleCoveredP95: report?.settle?.covered?.p95 ?? null,
    settleExactP95: report?.settle?.exact?.p95 ?? null,
    settleComboP95: report?.settle?.combo?.p95 ?? null,
    resumeP95: report?.resumeMs?.p95 ?? null,
    resumePlayP95: report?.resumePlayMs?.p95 ?? null,
    pausePlayP95: report?.pausePlayMs?.p95 ?? null,
    scrubPlayP95: report?.scrubPlayMs?.p95 ?? null,
    behindP95: report?.behindMs?.p95 ?? null,
    holdMsMax: report?.holdMs?.max ?? null,
    skippedMax: report?.skippedMax ?? null,
    holdsMax: report?.holdsMax ?? null,
    grayCoverageEnd: report?.grayCoverageEnd ?? null,
    faults:
      report?.faults ??
      (exitCode !== 0 ? ["(no report — harness crashed)"] : []),
    warnings: report?.warnings ?? [],
  };
}

async function writeSummary(rows) {
  const summary = {
    generatedAt: new Date().toISOString(),
    codec: CODEC,
    base: BASE,
    out: OUT,
    clips: CLIPS.length,
    done: rows.length,
    passed: rows.filter((r) => r.ok).length,
    failed: rows.filter((r) => !r.ok).length,
    pending: CLIPS.filter((c) => !rows.some((r) => r.clip === c)),
    rows,
  };

  await fs.mkdir(OUT, { recursive: true });
  const summaryPath = path.join(OUT, "summary.json");
  await fs.writeFile(summaryPath, JSON.stringify(summary, null, 2), "utf8");

  const lines = [
    `# Stress matrix ${CODEC.toUpperCase()} (${summary.generatedAt})`,
    "",
    `Base: \`${BASE}\``,
    `Progress: **${summary.done}** / ${summary.clips} (passed ${summary.passed}, failed ${summary.failed})`,
    "",
    "| clip | ok | openMs | pause→play | scrub→play | resume play | resume total | cold p95 | gray p95 | faults |",
    "| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | --- |",
  ];
  for (const row of rows) {
    const fault = (row.faults[0] ?? "").replace(/\|/g, "/").slice(0, 80);
    lines.push(
      `| \`${row.clip}\` | ${row.ok ? "yes" : "NO"} | ${row.openMs ?? "—"} | ${row.pausePlayP95 ?? "—"} | ${row.scrubPlayP95 ?? "—"} | ${row.resumePlayP95 ?? "—"} | ${row.resumeP95 ?? "—"} | ${row.settleColdP95 ?? "—"} | ${row.settleGrayP95 ?? "—"} | ${fault || "—"} |`,
    );
  }
  if (summary.pending.length) {
    lines.push("", "## Pending", "");
    for (const p of summary.pending) lines.push(`- \`${p}\``);
  }
  lines.push("");
  const mdPath = path.join(OUT, "summary.md");
  await fs.writeFile(mdPath, lines.join("\n"), "utf8");
  return { summaryPath, mdPath, summary };
}

async function loadExistingRows() {
  const rows = [];
  for (const clip of CLIPS) {
    const reportPath = path.join(OUT, `${clip}.json`);
    try {
      const report = JSON.parse(await fs.readFile(reportPath, "utf8"));
      const metaPath = path.join(OUT, `${clip}.meta.json`);
      let exitCode = 0;
      let elapsedMs = null;
      try {
        const meta = JSON.parse(await fs.readFile(metaPath, "utf8"));
        exitCode = meta.exitCode ?? 0;
        elapsedMs = meta.elapsedMs ?? null;
      } catch {
        // infer fail from faults / missing resume
        exitCode =
          (report.faults?.some((f) =>
            /Play still dead|reopen still errored|pageerror/.test(f),
          ) ||
            !(report.resumeMs?.n > 0)) &&
          report.faults?.length
            ? 1
            : 0;
        // Prefer explicit: if resume ok and no fatal, treat as pass
        if (report.resumeMs?.n > 0) {
          const fatal = (report.faults ?? []).filter((f) =>
            /Play still dead|reopen still errored|^pageerror:/.test(f),
          );
          exitCode = fatal.length ? 1 : 0;
        }
      }
      rows.push(rowFrom(clip, report, exitCode, elapsedMs));
    } catch {
      // missing
    }
  }
  return rows;
}

function runOneClip(clipStem) {
  const url = `${BASE}/${clipStem}.mp4`;
  const reportPath = path.resolve(OUT, `${clipStem}.json`);
  const logPath = path.resolve(OUT, `${clipStem}.log`);
  const metaPath = path.resolve(OUT, `${clipStem}.meta.json`);

  return new Promise(async (resolve) => {
    await fs.mkdir(OUT, { recursive: true });
    const log = await fs.open(logPath, "w");
    const t0 = Date.now();
    // Prefer direct Playwright CLI so we don't depend on `npm` being on PATH
    // (agent shells / minimal CI images).
    const playwrightCli = path.resolve(
      "node_modules",
      "@playwright",
      "test",
      "cli.js",
    );
    const child = spawn(
      process.execPath,
      [playwrightCli, "test", "e2e/player.stress.scrub-av.spec.ts"],
      {
        env: {
          ...process.env,
          WEBMEDIA_CLIP: url,
          WEBMEDIA_STRESS_REPORT_PATH: reportPath,
        },
        shell: false,
        stdio: ["ignore", "pipe", "pipe"],
      },
    );

    const write = async (buf) => {
      await log.write(buf);
      process.stdout.write(buf);
    };
    child.stdout.on("data", (d) => void write(d));
    child.stderr.on("data", (d) => void write(d));

    child.on("close", async (code) => {
      await log.close();
      const elapsedMs = Date.now() - t0;
      const exitCode = code ?? 1;
      await fs.mkdir(OUT, { recursive: true });
      await fs.writeFile(
        metaPath,
        JSON.stringify({ clip: clipStem, url, exitCode, elapsedMs }, null, 2),
        "utf8",
      );
      let report = null;
      try {
        report = JSON.parse(await fs.readFile(reportPath, "utf8"));
      } catch {
        /* harness crashed before flush */
      }
      resolve({ clip: clipStem, url, exitCode, elapsedMs, report });
    });
  });
}

async function main() {
  console.log(
    `stress matrix ${CODEC} → ${OUT} (${CLIPS.length} clips, one at a time)`,
  );
  console.log(`base: ${BASE}`);
  await fs.mkdir(OUT, { recursive: true });

  let rows = FORCE ? [] : await loadExistingRows();
  if (rows.length && !FORCE) {
    console.log(`resuming with ${rows.length} existing clip report(s)`);
    await writeSummary(rows);
  }

  for (let i = 0; i < CLIPS.length; i++) {
    const clip = CLIPS[i];
    if (!FORCE && rows.some((r) => r.clip === clip)) {
      console.log(`\n[${i + 1}/${CLIPS.length}] skip (have report): ${clip}`);
      continue;
    }

    console.log(`\n[${i + 1}/${CLIPS.length}] RUN ${clip}`);
    const result = await runOneClip(clip);
    const row = rowFrom(
      result.clip,
      result.report,
      result.exitCode,
      result.elapsedMs,
    );
    rows = rows.filter((r) => r.clip !== clip).concat(row);
    // Keep catalog order in summary
    rows.sort(
      (a, b) => CLIPS.indexOf(a.clip) - CLIPS.indexOf(b.clip),
    );

    const { summaryPath, mdPath, summary } = await writeSummary(rows);
    console.log(
      JSON.stringify(
        {
          clip: row.clip,
          ok: row.ok,
          openMs: row.openMs,
          resumeP95: row.resumeP95,
          behindP95: row.behindP95,
          faults: row.faults,
          progress: `${summary.done}/${summary.clips}`,
        },
        null,
        2,
      ),
    );
    console.log(`updated ${summaryPath}`);
    console.log(`updated ${mdPath}`);
  }

  const final = await writeSummary(rows);
  console.log(`\nDONE passed=${final.summary.passed} failed=${final.summary.failed}`);
  console.log(final.summaryPath);
  console.log(final.mdPath);
  process.exit(final.summary.failed > 0 ? 1 : 0);
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
