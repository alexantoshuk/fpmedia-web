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
  /** ~1 MB scenecut — light open/smoke when FullHD gop12 is too heavy. */
  scenecut: "avc_1080p_24_scenecut_bf0",
  /** AVC 1080p 24 fps GOP12 bf0 ~60 s AAC — default smoke / play / JKL / EOF. */
  gop12: "avc_1080p_24_gop12_bf0",
  /** AVC 1080p 24 fps GOP12 bf2 — B-frame reorder / scrub. */
  gop12Bf2: "avc_1080p_24_gop12_bf2",
  /** AVC 1080p 24 fps GOP48 bf0 — long-GOP play + scrub settle / baseline. */
  gop48: "avc_1080p_24_gop48_bf0",
  /** AVC 1080p 24 fps fMP4 GOP12 — progressive open / moof path. */
  fmp4: "avc_1080p_24_fmp4_gop12_bf0",
  /** HEVC 1080p 24 fps GOP12 — key-verify later. */
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
  [TestvideoClip.hevc]: "HEVC 1080p 24fps GOP12 bf0 ~60s AAC",
};

export function testvideoUrl(id: TestvideoClipId | string): string {
  const base = TESTVIDEO_BASE.replace(/\/$/, "");
  const stem = id.endsWith(".mp4") ? id.slice(0, -4) : id;
  return `${base}/${stem}.mp4`;
}
