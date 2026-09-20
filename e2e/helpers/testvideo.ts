/**
 * Canonical e2e media: [testvideo](https://github.com/alexantoshuk/testvideo) Release assets.
 *
 * Do not point probes at Downloads / ad-hoc local paths. In-repo tiny fixtures
 * (`public/`, `media/tests/fixtures/`) remain for smoke + Rust unit demux only.
 *
 * Override base: `TESTVIDEO_BASE=https://…/download/vX.Y.Z`
 */
export const TESTVIDEO_BASE =
  process.env.TESTVIDEO_BASE ??
  "https://github.com/alexantoshuk/testvideo/releases/download/v0.1.0";

/** Asset stem (no `.mp4`) — matches testvideo `manifest.json` expand rule. */
export const TestvideoClip = {
  /** ~1 MB scenecut — light open / variable keys. */
  scenecut: "avc_1080p_24_scenecut_bf0",
  /** AVC 1080p 24 fps GOP12 bf0 ~60 s AAC — default smoke / play / JKL / EOF. */
  gop12: "avc_1080p_24_gop12_bf0",
  /** AVC 1080p 24 fps GOP12 bf2 — B-frame reorder / scrub. */
  gop12Bf2: "avc_1080p_24_gop12_bf2",
  /** AVC 1080p 24 fps GOP48 bf0 — long-GOP play + scrub settle / baseline. */
  gop48: "avc_1080p_24_gop48_bf0",
  /** AVC 1080p 24 fps fMP4 GOP12 — progressive open / moof path. */
  fmp4: "avc_1080p_24_fmp4_gop12_bf0",
  /** AVC 1080p 60 fps GOP12 bf0 — high-fps clock / A‑V. */
  gop12_60: "avc_1080p_60_gop12_bf0",
  /** HEVC 1080p 24 fps GOP12 — key-verify later (`WEBMEDIA_MATRIX_HEVC=1`). */
  hevc: "hevc_1080p_24_gop12_bf0",
} as const;

export type TestvideoClipId = (typeof TestvideoClip)[keyof typeof TestvideoClip];

/** Human meta for logs / baselines (not a filesystem path). */
export const TestvideoMeta: Record<TestvideoClipId, string> = {
  [TestvideoClip.scenecut]:
    "AVC 1080p 24fps scenecut bf0 ~60s AAC (hue shifts @2s)",
  [TestvideoClip.gop12]: "AVC 1080p 24fps GOP12 bf0 ~60s AAC",
  [TestvideoClip.gop12Bf2]: "AVC 1080p 24fps GOP12 bf2 ~60s AAC",
  [TestvideoClip.gop48]: "AVC 1080p 24fps GOP48 bf0 ~60s AAC",
  [TestvideoClip.fmp4]: "AVC 1080p 24fps fMP4 GOP12 bf0 ~60s AAC",
  [TestvideoClip.gop12_60]: "AVC 1080p 60fps GOP12 bf0 ~60s AAC",
  [TestvideoClip.hevc]: "HEVC 1080p 24fps GOP12 bf0 ~60s AAC",
};

export function testvideoUrl(id: TestvideoClipId | string): string {
  const base = TESTVIDEO_BASE.replace(/\/$/, "");
  const stem = id.endsWith(".mp4") ? id.slice(0, -4) : id;
  return `${base}/${stem}.mp4`;
}

/** What the matrix row is meant to stress (profile / path tuning). */
export type MatrixPurpose =
  | "short-gop"
  | "b-frames"
  | "long-gop"
  | "fmp4-open"
  | "high-fps"
  | "variable-keys"
  | "hevc";

export type MatrixExercise = "open" | "step" | "seek-mid" | "scrub-mid";

export type MatrixRole = {
  id: string;
  clip: TestvideoClipId;
  purpose: MatrixPurpose;
  /** Soft open budget (GitHub + proxy); fMP4 needs more. */
  openTimeoutMs: number;
  expect: {
    width: number;
    height: number;
    /** Inclusive fps band from meta string. */
    fpsMin: number;
    fpsMax: number;
    /** Substring of codec string (`avc1` / `hvc1`). */
    codecIncludes: string;
    /** Status GOP label fragment (`GOP 12`, `GOP ~48`, …). Empty = skip. */
    gopStatusIncludes: string;
    playPolicy: string;
    hasAudio: boolean;
    minLastFrame: number;
  };
  exercise: MatrixExercise;
};

/**
 * Narrow Release subset for profile / path tuning — not the full 18-clip matrix.
 * HEVC included only when `WEBMEDIA_MATRIX_HEVC=1`.
 */
export const MATRIX_ROLES: MatrixRole[] = [
  {
    id: "gop12",
    clip: TestvideoClip.gop12,
    purpose: "short-gop",
    openTimeoutMs: 180_000,
    expect: {
      width: 1920,
      height: 1080,
      fpsMin: 23.5,
      fpsMax: 24.5,
      codecIncludes: "avc1",
      gopStatusIncludes: "GOP 12",
      playPolicy: "play:hd",
      hasAudio: true,
      minLastFrame: 1000,
    },
    exercise: "step",
  },
  {
    id: "gop12-bf2",
    clip: TestvideoClip.gop12Bf2,
    purpose: "b-frames",
    openTimeoutMs: 180_000,
    expect: {
      width: 1920,
      height: 1080,
      fpsMin: 23.5,
      fpsMax: 24.5,
      codecIncludes: "avc1",
      gopStatusIncludes: "GOP 12",
      playPolicy: "play:hd",
      hasAudio: true,
      minLastFrame: 1000,
    },
    exercise: "seek-mid",
  },
  {
    id: "gop48",
    clip: TestvideoClip.gop48,
    purpose: "long-gop",
    openTimeoutMs: 180_000,
    expect: {
      width: 1920,
      height: 1080,
      fpsMin: 23.5,
      fpsMax: 24.5,
      codecIncludes: "avc1",
      gopStatusIncludes: "GOP ~48",
      playPolicy: "play:hd",
      hasAudio: true,
      minLastFrame: 1000,
    },
    exercise: "scrub-mid",
  },
  {
    id: "fmp4",
    clip: TestvideoClip.fmp4,
    purpose: "fmp4-open",
    openTimeoutMs: 300_000,
    expect: {
      width: 1920,
      height: 1080,
      // Warm-open fps from first fragments can be ~23.5±ε before/after backfill.
      fpsMin: 23.0,
      fpsMax: 25.0,
      codecIncludes: "avc1",
      gopStatusIncludes: "GOP 12",
      playPolicy: "play:hd",
      hasAudio: true,
      minLastFrame: 1000,
    },
    exercise: "open",
  },
  {
    id: "gop12-60",
    clip: TestvideoClip.gop12_60,
    purpose: "high-fps",
    openTimeoutMs: 240_000,
    expect: {
      width: 1920,
      height: 1080,
      fpsMin: 59,
      fpsMax: 61,
      codecIncludes: "avc1",
      gopStatusIncludes: "GOP 12",
      playPolicy: "play:hd",
      hasAudio: true,
      minLastFrame: 2000,
    },
    exercise: "step",
  },
  {
    id: "scenecut",
    clip: TestvideoClip.scenecut,
    purpose: "variable-keys",
    openTimeoutMs: 120_000,
    expect: {
      width: 1920,
      height: 1080,
      fpsMin: 23.5,
      fpsMax: 24.5,
      codecIncludes: "avc1",
      // Variable keys — don't assert a fixed GOP label.
      gopStatusIncludes: "",
      playPolicy: "play:hd",
      hasAudio: true,
      minLastFrame: 100,
    },
    exercise: "seek-mid",
  },
];

export function matrixRolesForRun(): MatrixRole[] {
  const roles = [...MATRIX_ROLES];
  if (process.env.WEBMEDIA_MATRIX_HEVC === "1") {
    roles.push({
      id: "hevc",
      clip: TestvideoClip.hevc,
      purpose: "hevc",
      openTimeoutMs: 180_000,
      expect: {
        width: 1920,
        height: 1080,
        fpsMin: 23.5,
        fpsMax: 24.5,
        codecIncludes: "hvc1",
        gopStatusIncludes: "GOP 12",
        playPolicy: "play:hd",
        hasAudio: true,
        minLastFrame: 1000,
      },
      exercise: "open",
    });
  }
  return roles;
}
