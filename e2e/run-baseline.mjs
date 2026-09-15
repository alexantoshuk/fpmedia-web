/**
 * Run scrub baseline save/compare without cross-env.
 * Usage: node e2e/run-baseline.mjs save|compare
 */
import { spawnSync } from "node:child_process";
import path from "node:path";
import { fileURLToPath } from "node:url";

const mode = (process.argv[2] ?? "save").toLowerCase();
if (mode !== "save" && mode !== "compare") {
  console.error("usage: node e2e/run-baseline.mjs save|compare");
  process.exit(2);
}

process.env.WEBMEDIA_BASELINE_MODE = mode;

const result = spawnSync(
  process.platform === "win32" ? "npx.cmd" : "npx",
  [
    "playwright",
    "test",
    "e2e/perf.scrub.baseline.spec.ts",
    "--reporter=list",
  ],
  {
    stdio: "inherit",
    env: process.env,
    shell: process.platform === "win32",
    cwd: path.resolve(path.dirname(fileURLToPath(import.meta.url)), ".."),
  },
);

process.exit(result.status ?? 1);
