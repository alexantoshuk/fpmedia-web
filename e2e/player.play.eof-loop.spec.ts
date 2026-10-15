/**
 * Chrome play / EOF / loop seam (AVC 1080p 24fps GOP12 bf0, ~60 s).
 * Clip: testvideo `avc_1080p_24_gop12_bf0` via Trunk `/media/`.
 * Seeks near EOF so the probe does not wait the full duration.
 * Expects app on :3000.
 */
import { expect, test } from "@playwright/test";
import { DemoPlayer, parseStatus } from "./helpers/player";
import { TestvideoClip, testvideoUrl } from "./helpers/testvideo";

const CLIP_URL = testvideoUrl(TestvideoClip.gop12);

type Sample = {
  t: number;
  playhead: number | null;
  seconds: number | null;
  playing: boolean;
  looping: boolean;
  pipeline: string;
  status: string;
};

function parseHolds(pipeline: string): { count: number; ms: number } {
  const m = pipeline.match(/holds\s+(\d+)\/([\d.]+)ms/);
  if (!m) return { count: 0, ms: 0 };
  return { count: Number(m[1]), ms: Number(m[2]) };
}

function parseLagMs(pipeline: string): number | null {
  const m = pipeline.match(/lag\s+([\d.]+)ms/);
  return m ? Number(m[1]) : null;
}

test.describe("play/EOF/loop (Chrome, testvideo GOP12)", () => {
  test("measures mid-play, EOF, and loop-seam continuity", async ({ page }) => {
    test.setTimeout(240_000);

    const consoleNotes: string[] = [];
    page.on("console", (msg) => {
      const text = msg.text();
      if (
        msg.type() === "error" ||
        text.includes("flush/idle") ||
        text.includes("backpressure timed out")
      ) {
        consoleNotes.push(text);
      }
    });
    page.on("pageerror", (err) => consoleNotes.push(`pageerror ${err}`));

    const player = new DemoPlayer(page);
    await player.goto();
    await player.openUrl(CLIP_URL, 180_000);
    await expect(player.error).toHaveText("");

    const opened = await player.readStatus();
    expect(opened.ready).toBe(true);
    expect(opened.lastFrame ?? 0).toBeGreaterThan(50);

    const samples: Sample[] = [];
    const pushSample = async () => {
      const statusRaw = await player.status.innerText();
      const st = parseStatus(statusRaw);
      const pipeline = (await player.pipeline.innerText()).trim();
      samples.push({
        t: Date.now(),
        playhead: st.playhead,
        seconds: st.seconds,
        playing: st.playing,
        looping: st.looping,
        pipeline,
        status: statusRaw.trim(),
      });
    };

    // --- Pass A: seek near end, play to EOF without loop ---
    const last = opened.lastFrame ?? 0;
    const nearEof = Math.max(0, last - 90);
    await player.seekToFrame(nearEof);
    await player.play.click();
    await expect
      .poll(async () => (await player.readStatus()).playing, { timeout: 20_000 })
      .toBe(true);

    const deadlineA = Date.now() + 45_000;
    let sawNearEnd = false;
    while (Date.now() < deadlineA) {
      await pushSample();
      const st = samples[samples.length - 1]!;
      const ph = st.playhead ?? 0;
      if (ph >= last * 0.85) sawNearEnd = true;
      if (!st.playing && sawNearEnd) break;
      if (ph >= last) break;
      await page.waitForTimeout(100);
    }
    await pushSample();

    const passA = samples.slice();
    const holdsA = parseHolds(passA[passA.length - 1]?.pipeline ?? "");
    const mid = passA.filter(
      (s) =>
        s.playing &&
        (s.playhead ?? 0) > nearEof &&
        (s.playhead ?? 0) < last * 0.98,
    );
    let midRealtime = null as null | number;
    if (mid.length >= 2) {
      const a = mid[0]!;
      const b = mid[mid.length - 1]!;
      const dWall = (b.t - a.t) / 1000;
      const dMedia = (b.seconds ?? 0) - (a.seconds ?? 0);
      if (dWall > 0.5 && dMedia > 0.5) midRealtime = dMedia / dWall;
    }

    let eofGapMs: number | null = null;
    for (let i = 1; i < passA.length; i++) {
      const prev = passA[i - 1]!;
      const cur = passA[i]!;
      if (prev.playing && !cur.playing) {
        eofGapMs = cur.t - prev.t;
        break;
      }
    }

    await test.info().attach("pass-a-eof", {
      body: JSON.stringify(
        {
          opened: opened.raw,
          midRealtime,
          holdsA,
          eofGapMs,
          lastPipeline: passA[passA.length - 1]?.pipeline,
          lastStatus: passA[passA.length - 1]?.status,
          nearEnd: passA.slice(-15),
          consoleNotes: consoleNotes.slice(-40),
        },
        null,
        2,
      ),
      contentType: "application/json",
    });

    // --- Pass B: loop on, seek near end, play through one wrap ---
    samples.length = 0;
    if (!(await player.readStatus()).looping) {
      await player.loop.click();
    }
    await expect
      .poll(async () => (await player.readStatus()).looping, { timeout: 5_000 })
      .toBe(true);

    const nearEnd = Math.max(0, last - 30);
    await player.seekToFrame(nearEnd);
    await player.play.click();
    await expect
      .poll(async () => (await player.readStatus()).playing, { timeout: 15_000 })
      .toBe(true);

    const deadlineB = Date.now() + 40_000;
    let wrapped = false;
    let wrapAt: Sample | null = null;
    let beforeWrap: Sample | null = null;
    while (Date.now() < deadlineB) {
      await pushSample();
      const st = samples[samples.length - 1]!;
      const ph = st.playhead ?? 0;
      if (!wrapped && samples.length >= 2) {
        const prev = samples[samples.length - 2]!;
        const prevPh = prev.playhead ?? 0;
        if (prevPh > last * 0.7 && ph < last * 0.2 && st.playing) {
          wrapped = true;
          beforeWrap = prev;
          wrapAt = st;
          const afterDeadline = Date.now() + 1_500;
          while (Date.now() < afterDeadline) {
            await page.waitForTimeout(50);
            await pushSample();
          }
          break;
        }
      }
      await page.waitForTimeout(50);
    }

    const holdsB = parseHolds(samples[samples.length - 1]?.pipeline ?? "");
    let loopSeamWallMs: number | null = null;
    let loopSeamMediaJump: number | null = null;
    if (beforeWrap && wrapAt) {
      loopSeamWallMs = wrapAt.t - beforeWrap.t;
      loopSeamMediaJump =
        (beforeWrap.seconds ?? 0) - (wrapAt.seconds ?? 0);
    }

    let postWrapRealtime: number | null = null;
    const post = samples.filter((s) => {
      if (!wrapAt) return false;
      return s.t >= wrapAt.t && (s.playhead ?? 0) < last * 0.3;
    });
    if (post.length >= 3) {
      const a = post[0]!;
      const b = post[post.length - 1]!;
      const dWall = (b.t - a.t) / 1000;
      const dMedia = (b.seconds ?? 0) - (a.seconds ?? 0);
      if (dWall > 0.3) postWrapRealtime = dMedia / dWall;
    }

    const maxLag = samples
      .map((s) => parseLagMs(s.pipeline))
      .filter((v): v is number => v != null)
      .reduce((m, v) => Math.max(m, v), 0);

    const summary = {
      clipUrl: CLIP_URL,
      opened: opened.raw,
      passA: { midRealtime, holdsA, eofGapMs },
      passB: {
        wrapped,
        holdsB,
        loopSeamWallMs,
        loopSeamMediaJump,
        postWrapRealtime,
        maxLagMs: maxLag,
        beforeWrap,
        wrapAt,
        afterWrap: samples.slice(-20),
      },
      consoleNotes: consoleNotes.slice(-50),
    };

    await test.info().attach("eof-loop-summary", {
      body: JSON.stringify(summary, null, 2),
      contentType: "application/json",
    });
    console.log(
      "EOF/loop summary",
      JSON.stringify({
        midRealtime: summary.passA.midRealtime,
        holdsA: summary.passA.holdsA,
        eofGapMs: summary.passA.eofGapMs,
        holdsB: summary.passB.holdsB,
        loopSeamWallMs: summary.passB.loopSeamWallMs,
        postWrapRealtime: summary.passB.postWrapRealtime,
        maxLagMs: summary.passB.maxLagMs,
        wrapped: summary.passB.wrapped,
      }),
    );

    expect(consoleNotes.filter((n) => n.includes("flush/idle"))).toEqual([]);
    expect(wrapped, "did not observe loop wrap near EOF").toBe(true);
    expect(
      holdsB.ms,
      `underrun hold ms too high: ${holdsB.count}/${holdsB.ms}ms`,
    ).toBeLessThan(500);
    if (postWrapRealtime != null) {
      expect(
        postWrapRealtime,
        `post-wrap realtime ratio ${postWrapRealtime}`,
      ).toBeGreaterThan(0.7);
    }
  });
});
