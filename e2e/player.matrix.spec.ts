/**
 * Narrow testvideo Release matrix for ScrubPolicy / PlayPolicy / open-path tuning.
 *
 * Roles: GOP12, GOP12+bf2, GOP48, fMP4, 60 fps GOP12, scenecut.
 * Optional HEVC: `WEBMEDIA_MATRIX_HEVC=1`.
 *
 * Expects app on :3000 + network (`/proxy`).
 *   npm run e2e:matrix
 */
import { expect, test } from "@playwright/test";
import { DemoPlayer } from "./helpers/player";
import {
  TestvideoMeta,
  matrixRolesForRun,
  testvideoUrl,
  type MatrixRole,
} from "./helpers/testvideo";

function parseFpsFromMeta(meta: string): number | null {
  const m = meta.match(/([\d.]+)\s*fps/i);
  return m ? Number(m[1]) : null;
}

async function assertOpenExpectations(
  player: DemoPlayer,
  role: MatrixRole,
) {
  const status = await player.readStatus();
  const meta = (await player.meta.innerText()).trim();
  const snap = { status: status.raw, meta };

  expect(status.ready, JSON.stringify(snap)).toBe(true);
  await expect(player.error).toHaveText("");

  expect(meta, meta).toContain(`${role.expect.width}×${role.expect.height}`);
  expect(meta.toLowerCase(), meta).toContain(role.expect.codecIncludes);
  if (role.expect.hasAudio) {
    expect(meta, meta).toMatch(/\baudio\b/i);
  }

  const fps = parseFpsFromMeta(meta);
  expect(fps, `fps in meta: ${meta}`).not.toBeNull();
  expect(fps!).toBeGreaterThanOrEqual(role.expect.fpsMin);
  expect(fps!).toBeLessThanOrEqual(role.expect.fpsMax);

  expect(status.raw.toLowerCase(), status.raw).toContain(
    role.expect.playPolicy.toLowerCase(),
  );
  if (role.expect.gopStatusIncludes) {
    expect(status.raw, status.raw).toContain(role.expect.gopStatusIncludes);
  }

  expect(status.lastFrame ?? 0).toBeGreaterThan(role.expect.minLastFrame);
}

async function runExercise(player: DemoPlayer, role: MatrixRole) {
  const last = (await player.readStatus()).lastFrame ?? 0;
  switch (role.exercise) {
    case "open":
      return;
    case "step": {
      await player.next.click();
      await player.waitEngine(1, 15_000);
      await player.prev.click();
      await player.waitEngine(0, 15_000);
      return;
    }
    case "seek-mid": {
      const mid = Math.floor(last * 0.4);
      await player.seekToFrame(mid);
      const st = await player.readStatus();
      expect(st.engine).toBe(mid);
      expect(st.playhead).toBe(mid);
      // Cross a few presentation steps (bf2 reorder stress).
      await player.next.click();
      await player.waitEngine(mid + 1, 20_000);
      await player.next.click();
      await player.waitEngine(mid + 2, 20_000);
      return;
    }
    case "scrub-mid": {
      await player.scrubToFraction(0.45);
      await expect
        .poll(
          async () => {
            const s = await player.readStatus();
            return (
              s.playhead != null &&
              s.engine != null &&
              s.playhead === s.engine &&
              s.playhead > 10
            );
          },
          { timeout: 60_000, message: `${role.id}: scrub settle exact` },
        )
        .toBe(true);
      return;
    }
    default: {
      const _exhaustive: never = role.exercise;
      return _exhaustive;
    }
  }
}

test.describe("testvideo matrix (profile / path roles)", () => {
  for (const role of matrixRolesForRun()) {
    test(`${role.id} · ${role.purpose} · ${TestvideoMeta[role.clip]}`, async ({
      page,
    }) => {
      test.setTimeout(role.openTimeoutMs + 120_000);

      const consoleFaults: string[] = [];
      page.on("console", (msg) => {
        const t = msg.text();
        if (
          msg.type() === "error" ||
          t.includes("flush/idle") ||
          t.includes("backpressure timed out")
        ) {
          consoleFaults.push(t);
        }
      });
      page.on("pageerror", (err) => consoleFaults.push(String(err)));

      const player = new DemoPlayer(page);
      await player.goto();

      const url = testvideoUrl(role.clip);
      const openStart = Date.now();
      await player.openUrl(url, role.openTimeoutMs);
      const openMs = Date.now() - openStart;

      // fMP4 warm-open is ready with a short index; wait for bg backfill before
      // asserting full-clip expectations (minLastFrame / fps).
      if (role.expect.minLastFrame > 0) {
        await expect
          .poll(
            async () => (await player.readStatus()).lastFrame ?? 0,
            {
              timeout: role.openTimeoutMs,
              message: `${role.id}: wait fragment backfill (lastFrame)`,
            },
          )
          .toBeGreaterThan(role.expect.minLastFrame);
      }

      await assertOpenExpectations(player, role);
      await runExercise(player, role);

      const fault = consoleFaults.find(
        (e) =>
          e.includes("flush/idle") ||
          e.includes("backpressure timed out") ||
          e.includes("decode pump"),
      );
      expect(fault, fault).toBeUndefined();

      const report = {
        role: role.id,
        purpose: role.purpose,
        clip: role.clip,
        openMs,
        status: (await player.readStatus()).raw,
        meta: (await player.meta.innerText()).trim(),
      };
      console.log(JSON.stringify(report));
      await test.info().attach(`matrix-${role.id}`, {
        body: JSON.stringify(report, null, 2),
        contentType: "application/json",
      });
    });
  }
});
