import { spawnSync } from "node:child_process";
import type { Page } from "@playwright/test";

/** Coarse luma grid. Same relative cells on the 320×240 plate and on the stage buffer. */
export const SIG_W = 40;
export const SIG_H = 20;

const PLATE_W = 320;
const PLATE_H = 240;

export type FrameSig = number[];

const cache = new Map<string, FrameSig[]>();

function sigFromRgb(rgb: Buffer, w: number, h: number): FrameSig {
  // Top-left burn-in (f N, timecode, clip label). The rest of testsrc
  // moves too smoothly for a coarse grid to separate neighbors.
  const xB = Math.floor(w * 0.5);
  const yB = Math.floor(h * (110 / 240));
  const out: number[] = [];
  for (let ry = 0; ry < SIG_H; ry++) {
    const y0 = Math.floor((ry * yB) / SIG_H);
    const y1 = Math.max(y0 + 1, Math.floor(((ry + 1) * yB) / SIG_H));
    for (let rx = 0; rx < SIG_W; rx++) {
      const x0 = Math.floor((rx * xB) / SIG_W);
      const x1 = Math.max(x0 + 1, Math.floor(((rx + 1) * xB) / SIG_W));
      let sum = 0;
      let n = 0;
      for (let y = y0; y < y1; y++) {
        for (let x = x0; x < x1; x++) {
          const i = (y * w + x) * 3;
          sum += rgb[i] * 0.3 + rgb[i + 1] * 0.59 + rgb[i + 2] * 0.11;
          n++;
        }
      }
      out.push(n ? sum / n : 0);
    }
  }
  return out;
}

/** Decode every presentation frame of a fixture plate to a luma signature. */
export function loadFrameSigs(fixturePath: string): FrameSig[] {
  const hit = cache.get(fixturePath);
  if (hit) return hit;
  const ff = spawnSync(
    "ffmpeg",
    [
      "-v",
      "error",
      "-i",
      fixturePath,
      "-f",
      "rawvideo",
      "-pix_fmt",
      "rgb24",
      "pipe:1",
    ],
    { maxBuffer: 32 * 1024 * 1024 },
  );
  if (ff.status !== 0) {
    const err = ff.stderr?.toString() ?? "ffmpeg failed";
    throw new Error(err);
  }
  const raw = ff.stdout as Buffer;
  const frameBytes = PLATE_W * PLATE_H * 3;
  const n = Math.floor(raw.length / frameBytes);
  const sigs: FrameSig[] = [];
  for (let f = 0; f < n; f++) {
    sigs.push(
      sigFromRgb(raw.subarray(f * frameBytes, (f + 1) * frameBytes), PLATE_W, PLATE_H),
    );
  }
  if (sigs.length < 2) {
    throw new Error(`no frames decoded from ${fixturePath}`);
  }
  cache.set(fixturePath, sigs);
  return sigs;
}

function mse(a: FrameSig, b: FrameSig): number {
  let sum = 0;
  const n = Math.min(a.length, b.length);
  for (let i = 0; i < n; i++) {
    const d = a[i] - b[i];
    sum += d * d;
  }
  return sum / n;
}

export function bestFrame(
  sig: FrameSig,
  refs: FrameSig[],
): { frame: number; mse: number; second: number; secondMse: number } {
  let frame = 0;
  let best = Number.POSITIVE_INFINITY;
  let second = -1;
  let secondMse = Number.POSITIVE_INFINITY;
  for (let i = 0; i < refs.length; i++) {
    const m = mse(sig, refs[i]);
    if (m < best) {
      second = frame;
      secondMse = best;
      frame = i;
      best = m;
    } else if (m < secondMse) {
      second = i;
      secondMse = m;
    }
  }
  return { frame, mse: best, second, secondMse };
}

/** Luma grid of the visible stage canvas (idle playlist slot is `display: none`). */
export async function readStageSig(page: Page): Promise<FrameSig> {
  const sig = await page.evaluate(() => {
    const list = [
      ...document.querySelectorAll("canvas.fpvideo-canvas"),
    ] as HTMLCanvasElement[];
    const canvas = list.find((c) => c.offsetParent !== null && c.width > 0);
    if (!canvas) return null;
    const ctx = canvas.getContext("2d", { willReadFrequently: true });
    if (!ctx) return null;
    const w = canvas.width;
    const h = canvas.height;
    const data = ctx.getImageData(0, 0, w, h).data;
    const cols = 40;
    const rows = 20;
    const xB = Math.floor(w * 0.5);
    const yB = Math.floor(h * (110 / 240));
    const out: number[] = [];
    for (let ry = 0; ry < rows; ry++) {
      const y0 = Math.floor((ry * yB) / rows);
      const y1 = Math.max(y0 + 1, Math.floor(((ry + 1) * yB) / rows));
      for (let rx = 0; rx < cols; rx++) {
        const x0 = Math.floor((rx * xB) / cols);
        const x1 = Math.max(x0 + 1, Math.floor(((rx + 1) * xB) / cols));
        let sum = 0;
        let n = 0;
        for (let y = y0; y < y1; y++) {
          for (let x = x0; x < x1; x++) {
            const i = (y * w + x) * 4;
            sum += data[i] * 0.3 + data[i + 1] * 0.59 + data[i + 2] * 0.11;
            n++;
          }
        }
        out.push(n ? sum / n : 0);
      }
    }
    return out;
  });
  if (!sig) throw new Error("stage canvas not readable");
  return sig;
}
