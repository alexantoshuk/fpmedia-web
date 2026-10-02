import { chromium } from "@playwright/test";
import path from "node:path";

const CLIP =
  process.env.WEBMEDIA_CLIP ||
  "C:\\Users\\apaleshka\\Downloads\\webmedia-ab\\ChG_full_video_only.mp4";
const PLAY_MS = Number(process.env.PLAY_MS || 45000);
const BASE = process.env.BASE_URL || "http://127.0.0.1:3000";

const browser = await chromium.launch({
  channel: process.platform === "win32" ? "chrome" : undefined,
  headless: false,
  args: ["--autoplay-policy=no-user-gesture-required"],
});
const page = await browser.newPage();
await page.goto(BASE, { waitUntil: "domcontentloaded", timeout: 60_000 });
await page.getByTestId("player").waitFor({ state: "visible", timeout: 30_000 });
await page.getByTestId("file-input").setInputFiles(path.resolve(CLIP));

const statusEl = page.getByTestId("status");
const pipeEl = page.getByTestId("pipeline");
const jsonEl = page.getByTestId("pipeline-json");

while (true) {
  const t = (await statusEl.innerText()).trim();
  const m = t.match(/frame\s+\d+\s*\/\s*(\d+)/i);
  if (m && Number(m[1]) > 1000) break;
  await page.waitForTimeout(400);
}
console.log("OPENED:", (await statusEl.innerText()).trim());
await page.getByTestId("btn-play-pause").click();

while (true) {
  const t = (await statusEl.innerText()).trim();
  if (t.includes("playing") && !t.startsWith("Ready")) break;
  await page.waitForTimeout(200);
}
// discard prime / first second
await page.waitForTimeout(1000);

const samples = [];
const t0 = Date.now();
let prevSkip = null;
while (Date.now() - t0 < PLAY_MS) {
  const status = (await statusEl.innerText()).trim();
  const pipeline = (await pipeEl.innerText()).trim();
  let j = {};
  try { j = JSON.parse((await jsonEl.innerText()).trim() || "{}"); } catch {}
  const fpsM = status.match(/([\d.]+)\s*fps/);
  const held = j.held_len ?? (pipeline.match(/held\s+(\d+)/) ? Number(RegExp.$1) : null);
  const dq = j.decode_queue_size;
  const skip = j.frames_skipped ?? 0;
  const lag = j.last_behind_ms;
  const holds = j.underrun_hold_ms ?? 0;
  const holdN = j.underrun_holds ?? 0;
  const dSkip = prevSkip == null ? 0 : skip - prevSkip;
  prevSkip = skip;
  const row = {
    t: Math.round((Date.now() - t0) / 1000),
    fps: fpsM ? Number(fpsM[1]) : null,
    held,
    dq,
    skip,
    dSkip,
    lag: lag == null ? null : Math.round(lag),
    holdsMs: Math.round(holds),
    holdN,
  };
  samples.push(row);
  if (samples.length % 5 === 0) {
    console.log(
      `t=${row.t}s fps=${row.fps} held=${row.held} dq=${row.dq} dSkip=${row.dSkip} skip=${row.skip} lag=${row.lag} holds=${row.holdN}/${row.holdsMs}ms`,
    );
  }
  await page.waitForTimeout(500);
}

const fpsVals = samples.map((s) => s.fps).filter((x) => x != null);
const heldVals = samples.map((s) => s.held).filter((x) => typeof x === "number");
const lagVals = samples.map((s) => s.lag).filter((x) => typeof x === "number");
const dSkips = samples.map((s) => s.dSkip);
const last = samples[samples.length - 1];
const playSec = PLAY_MS / 1000;
const summary = {
  playSec,
  fps_end: last?.fps,
  fps_min: fpsVals.length ? Math.min(...fpsVals) : null,
  fps_avg: fpsVals.length ? +(fpsVals.reduce((a, b) => a + b, 0) / fpsVals.length).toFixed(2) : null,
  held_min: heldVals.length ? Math.min(...heldVals) : null,
  held_avg: heldVals.length ? +(heldVals.reduce((a, b) => a + b, 0) / heldVals.length).toFixed(2) : null,
  held_le1_frac: heldVals.length
    ? +((heldVals.filter((h) => h <= 1).length / heldVals.length) * 100).toFixed(1)
    : null,
  skip_total: last?.skip ?? 0,
  skip_per_sec: last ? +(last.skip / playSec).toFixed(2) : null,
  dSkip_max_per_500ms: dSkips.length ? Math.max(...dSkips) : null,
  lag_max: lagVals.length ? Math.max(...lagVals) : null,
  lag_p95: lagVals.length
    ? lagVals.sort((a, b) => a - b)[Math.floor(lagVals.length * 0.95)]
    : null,
  holds_n: last?.holdN ?? 0,
  holds_ms: last?.holdsMs ?? 0,
};
console.log("SERIES_SUMMARY:", JSON.stringify(summary, null, 2));
console.log(
  "VERDICT:",
  summary.holds_ms === 0 &&
    summary.skip_per_sec < 0.15 &&
    summary.held_min >= 2 &&
    (summary.fps_min ?? 0) >= 24
    ? "PASS_STRICT"
    : "FAIL_FEELS_LAGGY",
);

await browser.close();
process.exit(summary.holds_ms === 0 && summary.skip_per_sec < 0.15 ? 0 : 2);