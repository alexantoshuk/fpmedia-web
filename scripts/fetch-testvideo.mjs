/**
 * Download testvideo Release assets into demo-player/public/media/ for same-origin
 * progressive URL open (Trunk static serve — no proxy). Gitignored *.mp4.
 *
 * Usage (repo root):
 *   node scripts/fetch-testvideo.mjs              # default e2e set
 *   node scripts/fetch-testvideo.mjs gop12 fmp4   # aliases or full stems
 *   npm run media:fetch
 *
 * Env: TESTVIDEO_BASE=https://…/download/vX.Y.Z
 */
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const root = path.resolve(__dirname, "..");
const outDir = path.join(root, "demo-player", "public", "media");

const TESTVIDEO_BASE = (
  process.env.TESTVIDEO_BASE ??
  "https://github.com/alexantoshuk/testvideo/releases/download/v0.3.0"
).replace(/\/$/, "");

/** Alias → stem (no .mp4). Keep in sync with e2e/helpers/testvideo.ts. */
const ALIAS = {
  scenecut: "avc_1080p_25_scenecut_bf0",
  gop12: "avc_1080p_25_gop12_bf0",
  gop12Bf2: "avc_1080p_25_gop12_bf2",
  "gop12-bf2": "avc_1080p_25_gop12_bf2",
  gop48: "avc_1080p_25_gop48_bf0",
  fmp4: "avc_1080p_25_fmp4_gop12_bf0",
  gop12_60: "avc_1080p_60_gop12_bf0",
  "gop12-60": "avc_1080p_60_gop12_bf0",
  hevc: "hevc_1080p_25_gop12_bf0",
};

const DEFAULT = ["gop12", "gop12Bf2", "gop48", "fmp4", "gop12_60", "scenecut"];

function resolveStem(arg) {
  if (ALIAS[arg]) return ALIAS[arg];
  return arg.endsWith(".mp4") ? arg.slice(0, -4) : arg;
}

async function download(stem) {
  const url = `${TESTVIDEO_BASE}/${stem}.mp4`;
  const dest = path.join(outDir, `${stem}.mp4`);
  if (fs.existsSync(dest) && fs.statSync(dest).size > 0) {
    console.log(`skip  ${stem}.mp4 (exists)`);
    return;
  }
  console.log(`get   ${url}`);
  const res = await fetch(url, { redirect: "follow" });
  if (!res.ok) {
    throw new Error(`${url} → ${res.status} ${res.statusText}`);
  }
  const buf = Buffer.from(await res.arrayBuffer());
  fs.writeFileSync(dest, buf);
  console.log(`wrote ${path.relative(root, dest)} (${(buf.length / 1e6).toFixed(1)} MB)`);
}

fs.mkdirSync(outDir, { recursive: true });
const args = process.argv.slice(2);
const stems = (args.length ? args : DEFAULT).map(resolveStem);

for (const stem of stems) {
  await download(stem);
}
console.log("done → open /media/<stem>.mp4 in the demo (trunk serve)");
