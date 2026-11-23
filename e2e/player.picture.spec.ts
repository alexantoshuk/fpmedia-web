/**
 * Stage picture vs fixture burn-in (`f N` + clip label).
 *
 * Paused scrub and scrub-with-resume each run a slow drag, a fast drag, and
 * quick timeline clicks. While the finger is still, and again after a pause
 * release, the canvas is that aim. Resume release starts Play instead of
 * freezing the frame.
 *
 * Fixtures: `fixtures/fixture_*.mp4` (48 frames, labels short / bf / audio).
 */
import { expect, test, type Page } from "@playwright/test";
import { DemoPlayer, FIXTURES } from "./helpers/player";
import {
  bestFrame,
  loadFrameSigs,
  readStageSig,
  type FrameSig,
} from "./helpers/picture";

const SHORT = loadFrameSigs(FIXTURES.shortGop);
const BF = loadFrameSigs(FIXTURES.withBFrames);
const AUDIO = loadFrameSigs(FIXTURES.withAudio);
const PLATES = [SHORT, BF, AUDIO] as const;

async function readLocalFrame(player: DemoPlayer): Promise<number> {
  const status = await player.readStatus();
  expect(status.playhead, status.raw).not.toBeNull();
  return status.playhead as number;
}

async function trackPoint(player: DemoPlayer, fraction: number) {
  await player.scrubber.scrollIntoViewIfNeeded();
  const box = await player.scrubber.boundingBox();
  if (!box) throw new Error("scrubber has no bounding box");
  const clamped = Math.min(0.96, Math.max(0.04, fraction));
  return {
    y: box.y + box.height / 2,
    x: box.x + box.width * clamped,
  };
}

/** Drag from `from` to `to`. `stepDelay` 0 is the fast drag; a pause is the slow one. */
async function dragHold(
  page: Page,
  player: DemoPlayer,
  from: number,
  to: number,
  stepDelay: number,
) {
  const start = await trackPoint(player, from);
  const end = await trackPoint(player, to);
  await page.mouse.move(start.x, start.y);
  await page.mouse.down();
  const steps = 8;
  for (let i = 1; i <= steps; i++) {
    const x = start.x + ((end.x - start.x) * i) / steps;
    await page.mouse.move(x, end.y);
    if (stepDelay > 0) await page.waitForTimeout(stepDelay);
  }
}

async function clickAt(page: Page, player: DemoPlayer, fraction: number) {
  const point = await trackPoint(player, fraction);
  await page.mouse.move(point.x, point.y);
  await page.mouse.down();
  await page.mouse.up();
}

/** Held finger: burn-in matches the live aim. Returns that local frame. */
async function expectHeld(
  page: Page,
  player: DemoPlayer,
  refs: FrameSig[],
  label: string,
): Promise<number> {
  let held = -1;
  await expect
    .poll(
      async () => {
        const status = await player.readStatus();
        if (status.playhead == null) return "wait";
        const hit = bestFrame(await readStageSig(page), refs);
        if (hit.frame !== status.playhead) return `${hit.frame}:${status.playhead}`;
        held = status.playhead;
        return `${hit.frame}:${status.playhead}`;
      },
      { timeout: 8_000, message: `${label}: held picture is not the aim` },
    )
    .toMatch(/^(\d+):\1$/);
  return held;
}

async function releasePaused(
  page: Page,
  player: DemoPlayer,
  refs: FrameSig[],
  held: number,
  label: string,
) {
  await page.mouse.up();
  await expect
    .poll(async () => bestFrame(await readStageSig(page), refs).frame, {
      timeout: 4_000,
      message: `${label}: release changed the picture`,
    })
    .toBe(held);
  expect(await readLocalFrame(player), `${label}: playhead after release`).toBe(
    held,
  );
  expect((await player.readStatus()).playing, `${label}: stayed paused`).toBe(
    false,
  );
}

async function releaseResume(page: Page, player: DemoPlayer, label: string) {
  await page.mouse.up();
  await expect
    .poll(async () => (await player.readStatus()).playing, {
      timeout: 4_000,
      message: `${label}: release did not resume play`,
    })
    .toBe(true);
}

test.describe("stage picture (fixture burn-in)", () => {
  test.describe.configure({ timeout: 180_000 });

  test("single paused: slow drag, fast drag, quick clicks", async ({ page }) => {
    const player = new DemoPlayer(page);
    await player.goto();
    await player.openFixture(FIXTURES.shortGop);

    await dragHold(page, player, 0.15, 0.7, 50);
    const slow = await expectHeld(page, player, SHORT, "slow drag");
    await releasePaused(page, player, SHORT, slow, "slow drag");

    await dragHold(page, player, 0.7, 0.3, 0);
    const fast = await expectHeld(page, player, SHORT, "fast drag");
    await releasePaused(page, player, SHORT, fast, "fast drag");

    // Second fast pass is the warm Exact cache.
    await dragHold(page, player, 0.3, 0.55, 0);
    const cached = await expectHeld(page, player, SHORT, "exact cache");
    await releasePaused(page, player, SHORT, cached, "exact cache");

    for (const fraction of [0.2, 0.5, 0.8]) {
      await clickAt(page, player, fraction);
    }
    await expect
      .poll(
        async () => {
          const aim = await readLocalFrame(player);
          const hit = bestFrame(await readStageSig(page), SHORT);
          return `${hit.frame}:${aim}`;
        },
        { timeout: 8_000, message: "quick click did not settle on its frame" },
      )
      .toMatch(/^(\d+):\1$/);
  });

  test("single resume: slow drag, fast drag, quick clicks", async ({ page }) => {
    const player = new DemoPlayer(page);
    await player.goto();
    await player.openFixture(FIXTURES.shortGop);
    await player.play.click();
    await expect
      .poll(async () => (await player.readStatus()).playing)
      .toBe(true);

    await dragHold(page, player, 0.2, 0.65, 50);
    await expectHeld(page, player, SHORT, "resume slow drag");
    await releaseResume(page, player, "resume slow drag");

    await dragHold(page, player, 0.65, 0.25, 0);
    await expectHeld(page, player, SHORT, "resume fast drag");
    await releaseResume(page, player, "resume fast drag");

    for (const fraction of [0.3, 0.55, 0.75]) {
      await clickAt(page, player, fraction);
    }
    await expect
      .poll(async () => (await player.readStatus()).playing, {
        timeout: 4_000,
        message: "quick clicks while playing did not resume",
      })
      .toBe(true);
  });

  test("playlist paused: slow drag, fast drag, quick clicks across clips", async ({
    page,
  }) => {
    const player = new DemoPlayer(page);
    await player.goto();
    await player.fileInput.setInputFiles([
      FIXTURES.shortGop,
      FIXTURES.withBFrames,
      FIXTURES.withAudio,
    ]);
    await player.waitReady(90_000);

    async function liveClip(): Promise<number> {
      const pos = await player.page.getByTestId("position-display").innerText();
      const fromPos = /\[(\d+)\]/.exec(pos);
      if (fromPos) return Number(fromPos[1]);
      const meta = await player.meta.innerText();
      return Number(/clip\s+(\d+)\//.exec(meta)?.[1] ?? "0");
    }

    async function refsForLive(): Promise<FrameSig[] | null> {
      const clip = await liveClip();
      return PLATES[clip - 1] ?? null;
    }

    await dragHold(page, player, 0.05, 0.5, 40);
    const slowRefs = await refsForLive();
    if (!slowRefs) throw new Error("no plate after slow drag");
    const slow = await expectHeld(page, player, slowRefs, "playlist slow");
    await releasePaused(page, player, slowRefs, slow, "playlist slow");
    await expect(player.page.getByTestId("position-display")).toContainText("[2]");

    await dragHold(page, player, 0.5, 0.84, 0);
    const fastRefs = await refsForLive();
    if (!fastRefs) throw new Error("no plate after fast drag");
    const fast = await expectHeld(page, player, fastRefs, "playlist fast");
    await releasePaused(page, player, fastRefs, fast, "playlist fast");
    await expect(player.page.getByTestId("position-display")).toContainText("[3]");

    for (const fraction of [0.1, 0.5, 0.84]) {
      await clickAt(page, player, fraction);
    }
    await expect
      .poll(
        async () => {
          const status = await player.readStatus();
          const refs = await refsForLive();
          if (!refs || status.playhead == null) return "wait";
          const hit = bestFrame(await readStageSig(page), refs);
          return `${hit.frame}:${status.playhead}`;
        },
        {
          timeout: 12_000,
          message: "playlist quick click did not settle on that clip",
        },
      )
      .toMatch(/^(\d+):\1$/);
    await expect(player.page.getByTestId("position-display")).toContainText("[3]");
  });

  test("playlist resume: slow drag, fast drag, quick clicks", async ({ page }) => {
    const player = new DemoPlayer(page);
    await player.goto();
    await player.fileInput.setInputFiles([
      FIXTURES.shortGop,
      FIXTURES.withBFrames,
      FIXTURES.withAudio,
    ]);
    await player.waitReady(90_000);
    await player.play.click();
    await expect
      .poll(async () => (await player.readStatus()).playing)
      .toBe(true);

    await dragHold(page, player, 0.08, 0.4, 50);
    {
      const meta = await player.meta.innerText();
      const clip = Number(/clip\s+(\d+)\//.exec(meta)?.[1] ?? "0");
      const refs = PLATES[clip - 1];
      if (!refs) throw new Error(`no plate for ${meta}`);
      await expectHeld(page, player, refs, "playlist resume slow");
      await releaseResume(page, player, "playlist resume slow");
    }

    await dragHold(page, player, 0.4, 0.8, 0);
    const meta = await player.meta.innerText();
    const clip = Number(/clip\s+(\d+)\//.exec(meta)?.[1] ?? "0");
    const refs = PLATES[clip - 1];
    if (!refs) throw new Error(`no plate for ${meta}`);
    await expectHeld(page, player, refs, "playlist resume drag");
    await releaseResume(page, player, "playlist resume drag");

    for (const fraction of [0.2, 0.7]) {
      await clickAt(page, player, fraction);
    }
    await expect
      .poll(async () => (await player.readStatus()).playing, {
        timeout: 4_000,
        message: "playlist quick clicks did not resume play",
      })
      .toBe(true);
  });
});
