import { expect, type Locator, type Page } from "@playwright/test";
import path from "node:path";
import { resolveClip, type ResolvedClip } from "./clip";

export const FIXTURES = {
  shortGop: path.resolve("demo-player/public/fixture_short_gop.mp4"),
  withBFrames: path.resolve("demo-player/public/fixture_with_b_frames.mp4"),
  withAudio: path.resolve("demo-player/public/fixture_with_audio.mp4"),
} as const;

/** Zone on the scrubber strip (product gray / dim green / bright green). */
export type ScrubZone = "cold" | "gray" | "covered" | "exact";

export type ReadyFrameMarkJson = {
  frame: number;
  end: number;
  kind: "Covered" | "Exact";
};

/** Parsed `data-testid="pipeline-json"` payload from demo player UI. */
export type PipelineJson = {
  fetch_file_ranges: [number, number][];
  ready_frames: ReadyFrameMarkJson[];
  last_behind_ms: number | null;
  underrun_holds: number;
  underrun_hold_ms: number;
  frames_skipped: number;
  held_len: number | null;
  decode_queue_size: number | null;
  scrub_busy: boolean;
  cache_frames: number;
  cache_chunks: number;
  presentation_len?: number;
  presentation_cap?: number;
};

function inRanges(fraction: number, ranges: [number, number][]): boolean {
  for (const [a, b] of ranges) {
    if (fraction >= a && fraction <= b) return true;
  }
  return false;
}

/** Classify scrubber fraction using FetchMap + densify marks (Exact > Covered > gray > cold). */
export function classifyFraction(
  fraction: number,
  pipe: PipelineJson,
  lastFrame: number,
): ScrubZone {
  const max = Math.max(1, lastFrame);
  const frame = Math.round(fraction * max);
  for (const m of pipe.ready_frames) {
    if (m.kind === "Exact" && frame >= m.frame && frame <= m.end) {
      return "exact";
    }
  }
  for (const m of pipe.ready_frames) {
    if (m.kind === "Covered" && frame >= m.frame && frame <= m.end) {
      return "covered";
    }
  }
  if (inRanges(fraction, pipe.fetch_file_ranges)) return "gray";
  return "cold";
}

/** First scrubber fraction (0…1) matching `zone`, or null if none in the sample grid. */
export function pickZoneFraction(
  pipe: PipelineJson,
  lastFrame: number,
  zone: ScrubZone,
  opts?: { prefer?: "mid" | "edge"; samples?: number },
): number | null {
  const samples = opts?.samples ?? 40;
  const hits: number[] = [];
  for (let i = 1; i < samples; i++) {
    const f = i / samples;
    if (classifyFraction(f, pipe, lastFrame) === zone) hits.push(f);
  }
  if (hits.length === 0) return null;
  if (opts?.prefer === "edge") return hits[0]!;
  return hits[Math.floor(hits.length / 2)]!;
}

export function grayCoverageRatio(pipe: PipelineJson): number {
  let covered = 0;
  for (const [a, b] of pipe.fetch_file_ranges) {
    covered += Math.max(0, b - a);
  }
  return Math.min(1, covered);
}

/** Parsed ready-state status line, e.g. `frame 3 / 29 · … · 0.100s · playing`. */
export type PlayerStatus = {
  raw: string;
  playhead: number | null;
  engine: number | null;
  lastFrame: number | null;
  seconds: number | null;
  playing: boolean;
  looping: boolean;
  ready: boolean;
};

export function parseStatus(raw: string): PlayerStatus {
  const text = raw.trim();
  const frameMatch = text.match(
    /frame\s+(\d+)(?:\s*\(eng\s+(\d+)\))?\s*\/\s*(\d+)/,
  );
  const secondsMatch = text.match(/·\s*([\d.]+)s/);
  return {
    raw: text,
    playhead: frameMatch ? Number(frameMatch[1]) : null,
    engine: frameMatch?.[2] != null ? Number(frameMatch[2]) : frameMatch
      ? Number(frameMatch[1])
      : null,
    lastFrame: frameMatch ? Number(frameMatch[3]) : null,
    seconds: secondsMatch ? Number(secondsMatch[1]) : null,
    playing: /\bplaying\b/.test(text),
    looping: /\bloop\b/.test(text),
    ready: frameMatch != null,
  };
}

export class DemoPlayer {
  readonly root: Locator;
  readonly status: Locator;
  readonly pipeline: Locator;
  readonly error: Locator;
  readonly meta: Locator;
  readonly scrubber: Locator;
  readonly scrubberProgress: Locator;
  readonly frameInput: Locator;
  readonly fileInput: Locator;
  readonly play: Locator;
  readonly pause: Locator;
  readonly loop: Locator;
  readonly prev: Locator;
  readonly next: Locator;
  readonly playbackRate: Locator;
  readonly pipelineJson: Locator;

  constructor(readonly page: Page) {
    this.root = page.getByTestId("player");
    this.status = page.getByTestId("status");
    this.pipeline = page.getByTestId("pipeline");
    this.pipelineJson = page.getByTestId("pipeline-json");
    this.error = page.getByTestId("error");
    this.meta = page.getByTestId("meta");
    this.scrubber = page.getByTestId("scrubber");
    this.scrubberProgress = page.getByTestId("scrubber-progress");
    this.frameInput = page.getByTestId("frame-input");
    this.fileInput = page.getByTestId("file-input");
    this.play = page.getByTestId("btn-play-pause");
    this.pause = page.getByTestId("btn-play-pause");
    this.loop = page.getByTestId("btn-loop");
    this.prev = page.getByTestId("btn-prev");
    this.next = page.getByTestId("btn-next");
    this.playbackRate = page.getByTestId("playback-rate");
  }

  async goto() {
    await this.page.goto("/");
    await expect(this.root).toBeVisible();
  }

  async openFixture(fixturePath: string, timeout = 60_000) {
    await this.fileInput.setInputFiles(fixturePath);
    await this.waitReady(timeout);
  }

  /**
   * Open via demo "Load Remote URL" (`window.prompt` → `open_demo_url`).
   *
   * Visible chrome path — the off-screen `url-input` / `btn-open-url` hooks are
   * outside the viewport and Playwright will not click them even with `force`.
   * Local: `/media/…` from `demo-player/public/media/` after `npm run media:fetch`.
   */
  async openUrl(url: string, timeout = 120_000) {
    this.page.once("dialog", async (dialog) => {
      await dialog.accept(url);
    });
    await this.page.getByTestId("btn-load-url").click();
    await this.waitReady(timeout);
  }

  /** Seed `empty URL` via "Load Remote URL" without dropping the open clip. */
  async openEmptyUrl() {
    this.page.once("dialog", async (dialog) => {
      await dialog.accept("");
    });
    await this.page.getByTestId("btn-load-url").click();
  }

  /**
   * Open from `WEBMEDIA_CLIP` (or explicit override): URL, local file, or catalog alias.
   * See `helpers/clip.ts`.
   */
  async openClip(
    clipOrOverride?: ResolvedClip | string,
    timeout = 180_000,
  ): Promise<ResolvedClip> {
    const clip =
      typeof clipOrOverride === "object" && clipOrOverride != null
        ? clipOrOverride
        : resolveClip(
            typeof clipOrOverride === "string" ? clipOrOverride : undefined,
          );
    if (clip.kind === "file") {
      await this.openFixture(clip.value, timeout);
    } else {
      await this.openUrl(clip.value, timeout);
    }
    return clip;
  }

  async readPipelineJson(): Promise<PipelineJson> {
    const raw = (await this.pipelineJson.innerText()).trim();
    if (!raw || raw === "{}") {
      return {
        fetch_file_ranges: [],
        ready_frames: [],
        last_behind_ms: null,
        underrun_holds: 0,
        underrun_hold_ms: 0,
        frames_skipped: 0,
        held_len: null,
        decode_queue_size: null,
        scrub_busy: false,
        cache_frames: 0,
        cache_chunks: 0,
      };
    }
    return JSON.parse(raw) as PipelineJson;
  }

  async waitReady(timeout = 20_000) {
    await expect
      .poll(async () => parseStatus(await this.status.innerText()).ready, {
        timeout,
        message: "player did not become ready (status frame N / M)",
      })
      .toBe(true);
    await expect(this.error).toHaveText("");
  }

  async readStatus(): Promise<PlayerStatus> {
    return parseStatus(await this.status.innerText());
  }

  /**
   * Latch whether the status line ever shows `playing` (catches Play windows
   * shorter than the `expect.poll` interval, e.g. Play from the last frame).
   * Returns a reader for the latch.
   */
  async watchPlaying(): Promise<() => Promise<boolean>> {
    await this.status.evaluate((el) => {
      const w = window as unknown as { __fpmSawPlaying?: boolean };
      const check = () => {
        if (/\bplaying\b/.test(el.textContent ?? "")) w.__fpmSawPlaying = true;
      };
      w.__fpmSawPlaying = false;
      check();
      new MutationObserver(check).observe(el, {
        childList: true,
        subtree: true,
        characterData: true,
      });
    });
    return () =>
      this.page.evaluate(
        () => (window as unknown as { __fpmSawPlaying?: boolean }).__fpmSawPlaying === true,
      );
  }

  /** Parsed rate badge (`1.00x`, `-2.00x`). */
  async readPlaybackRate(): Promise<number> {
    const text = (await this.playbackRate.innerText()).trim();
    const m = text.match(/(-?[\d.]+)\s*x/i);
    if (!m) throw new Error(`playback-rate badge unreadable: ${text}`);
    return Number(m[1]);
  }

  async waitPlaybackRate(rate: number, timeout = 5_000) {
    await expect
      .poll(async () => (await this.readPlaybackRate()).toFixed(2), {
        timeout,
        message: `playback rate did not reach ${rate}`,
      })
      .toBe(rate.toFixed(2));
  }

  async waitPlayhead(frame: number, timeout = 10_000) {
    await expect
      .poll(async () => (await this.readStatus()).playhead, {
        timeout,
        message: `playhead did not reach frame ${frame}`,
      })
      .toBe(frame);
  }

  async waitEngine(frame: number, timeout = 15_000) {
    await expect
      .poll(async () => (await this.readStatus()).engine, {
        timeout,
        message: `engine frame did not reach ${frame}`,
      })
      .toBe(frame);
  }

  /** Progress bar width as percent of track (0–100). */
  async scrubberPercent(): Promise<number> {
    const width = await this.scrubberProgress.evaluate((el) => {
      const style = getComputedStyle(el);
      const w = style.width;
      if (w.endsWith("%")) return Number.parseFloat(w);
      const parent = el.parentElement;
      if (!parent) return 0;
      const pw = parent.getBoundingClientRect().width;
      if (pw <= 0) return 0;
      return (el.getBoundingClientRect().width / pw) * 100;
    });
    return width;
  }

  async ariaPlayhead(): Promise<number> {
    return this.scrubber.evaluate((el) => {
      const data = el.getAttribute("data-playhead");
      if (data != null && data !== "") return Number(data);
      const attr = el.getAttribute("aria-valuenow");
      if (attr != null && attr !== "") return Number(attr);
      const prop = (el as HTMLElement).ariaValueNow;
      if (prop != null && prop !== "") return Number(prop);
      return NaN;
    });
  }

  /**
   * Click scrubber at a fraction of its width (0 = start, 1 = end).
   * Dispatches pointerdown/move/up so settle runs.
   */
  async scrubToFraction(fraction: number) {
    const box = await this.scrubber.boundingBox();
    if (!box) throw new Error("scrubber has no bounding box");
    const x = box.x + box.width * Math.min(1, Math.max(0, fraction));
    const y = box.y + box.height / 2;
    await this.page.mouse.move(x, y);
    await this.page.mouse.down();
    await this.page.mouse.move(x + 1, y);
    await this.page.mouse.up();
  }

  async seekToFrame(frame: number, timeout = 60_000) {
    // One seek only: fill + blur fires `change` → `seek_frame`. Do **not** also
    // click Seek (second enqueue cancelled the first / left soft-cold eng≪aim).
    await this.frameInput.fill(String(frame));
    await this.frameInput.blur();
    await this.waitEngine(frame, timeout);
  }

  async snapshot() {
    const status = await this.readStatus();
    const pipe = await this.readPipelineJson();
    const dense = pipe.scrub_busy ? " · dense" : "";
    const frameCache = `${pipe.cache_frames}f / ${pipe.cache_chunks}c${dense}`;
    const qLen = pipe.presentation_len ?? 0;
    const qCap = pipe.presentation_cap ?? 0;
    return {
      status,
      ariaPlayhead: await this.ariaPlayhead(),
      scrubberPercent: await this.scrubberPercent(),
      pipeline: (await this.pipeline.innerText()).trim(),
      meta: (await this.meta.innerText()).trim(),
      prefetchQueue: `${qLen}/${qCap}`,
      prefetchAhead: "",
      frameCache,
      error: (await this.error.innerText()).trim(),
    };
  }

  /** Densify marker frame count from hidden `pipeline-json` (`cache_frames`). */
  async denseCacheFrames(): Promise<number> {
    return (await this.readPipelineJson()).cache_frames;
  }

  /** True while densify fill owns the scrub decoder. */
  async scrubBusy(): Promise<boolean> {
    return (await this.readPipelineJson()).scrub_busy;
  }
}
