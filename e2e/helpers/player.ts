import { expect, type Locator, type Page } from "@playwright/test";
import path from "node:path";

export const FIXTURES = {
  shortGop: path.resolve("public/short_gop.mp4"),
  withBFrames: path.resolve("public/with_b_frames.mp4"),
  withAudio: path.resolve("public/with_audio.mp4"),
} as const;

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
  readonly seek: Locator;
  readonly playbackRate: Locator;
  readonly prefetchQueue: Locator;
  readonly prefetchAhead: Locator;
  readonly frameCache: Locator;

  constructor(readonly page: Page) {
    this.root = page.getByTestId("player");
    this.status = page.getByTestId("status");
    this.pipeline = page.getByTestId("pipeline");
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
    this.seek = page.getByTestId("btn-seek");
    this.playbackRate = page.getByTestId("playback-rate");
    this.prefetchQueue = page.getByTestId("meter-prefetch-queue-detail");
    this.prefetchAhead = page.getByTestId("meter-prefetch-ahead-detail");
    this.frameCache = page.getByTestId("meter-frame-cache-detail");
  }

  async goto() {
    await this.page.goto("/");
    await expect(this.root).toBeVisible();
  }

  async openFixture(fixturePath: string, timeout = 60_000) {
    await this.fileInput.setInputFiles(fixturePath);
    await this.waitReady(timeout);
  }

  /** Open via demo URL bar (rewrites remote → same-origin `/proxy`). */
  async openUrl(url: string, timeout = 120_000) {
    await this.page.getByTestId("url-input").fill(url);
    await this.page.getByTestId("btn-open-url").click();
    await this.waitReady(timeout);
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

  async seekToFrame(frame: number) {
    await this.frameInput.fill(String(frame));
    await this.frameInput.blur();
    // change event fires on blur after edit; Seek button also commits current value
    await this.seek.click();
    await this.waitEngine(frame);
  }

  async snapshot() {
    const status = await this.readStatus();
    return {
      status,
      ariaPlayhead: await this.ariaPlayhead(),
      scrubberPercent: await this.scrubberPercent(),
      pipeline: (await this.pipeline.innerText()).trim(),
      meta: (await this.meta.innerText()).trim(),
      prefetchQueue: (await this.prefetchQueue.innerText()).trim(),
      prefetchAhead: (await this.prefetchAhead.innerText()).trim(),
      frameCache: (await this.frameCache.innerText()).trim(),
      error: (await this.error.innerText()).trim(),
    };
  }

  /** Parsed `Nf / Mc` from the frame-cache meter (`12f / 1c · …`). */
  async denseCacheFrames(): Promise<number> {
    const text = (await this.frameCache.innerText()).trim();
    const m = text.match(/^(\d+)f\s*\//);
    return m ? Number(m[1]) : 0;
  }
}
