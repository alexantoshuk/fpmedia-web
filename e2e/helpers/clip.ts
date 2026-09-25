/**
 * Resolve the clip under test from `WEBMEDIA_CLIP` (or an explicit override).
 *
 * Accepts:
 *   - catalog alias: `gop12`, `gop48`, `fmp4`, `scenecut`, `gop12Bf2`, `gop12_60`, `hevc`
 *   - testvideo stem: `avc_1080p_24_gop48_bf0` (with or without `.mp4`)
 *   - HTTPS/HTTP URL (demo `/proxy`)
 *   - local filesystem path to an `.mp4`
 *
 * Default: `gop48` (hardest scrub). Override base with `TESTVIDEO_BASE`.
 */
import fs from "node:fs";
import path from "node:path";
import {
  TestvideoClip,
  TestvideoMeta,
  testvideoUrl,
  type TestvideoClipId,
} from "./testvideo";

export type ClipKind = "url" | "file";

export type ResolvedClip = {
  kind: ClipKind;
  /** Absolute path or remote URL passed to the demo. */
  value: string;
  label: string;
  /** Raw WEBMEDIA_CLIP / override before resolution. */
  raw: string;
};

const ALIAS_TO_ID: Record<string, TestvideoClipId> = {
  gop12: TestvideoClip.gop12,
  gop12bf2: TestvideoClip.gop12Bf2,
  "gop12-bf2": TestvideoClip.gop12Bf2,
  gop48: TestvideoClip.gop48,
  fmp4: TestvideoClip.fmp4,
  scenecut: TestvideoClip.scenecut,
  gop12_60: TestvideoClip.gop12_60,
  "gop12-60": TestvideoClip.gop12_60,
  hevc: TestvideoClip.hevc,
};

function looksLikeUrl(s: string): boolean {
  return /^https?:\/\//i.test(s);
}

function looksLikePath(s: string): boolean {
  if (looksLikeUrl(s)) return false;
  if (/^[a-zA-Z]:[\\/]/.test(s) || s.startsWith("\\\\") || s.startsWith("/")) {
    return true;
  }
  return s.endsWith(".mp4") && (s.includes("/") || s.includes("\\"));
}

/** Resolve clip from env or explicit override. */
export function resolveClip(override?: string): ResolvedClip {
  const raw = (override ?? process.env.WEBMEDIA_CLIP ?? "gop48").trim();
  if (!raw) {
    throw new Error("WEBMEDIA_CLIP is empty");
  }

  if (looksLikeUrl(raw)) {
    const stem = raw.split("/").pop() ?? raw;
    return { kind: "url", value: raw, label: stem, raw };
  }

  if (looksLikePath(raw)) {
    const abs = path.resolve(raw);
    if (!fs.existsSync(abs)) {
      throw new Error(`WEBMEDIA_CLIP file not found: ${abs}`);
    }
    return {
      kind: "file",
      value: abs,
      label: path.basename(abs),
      raw,
    };
  }

  const aliasKey = raw.toLowerCase().replace(/\.mp4$/i, "");
  const aliasId = ALIAS_TO_ID[aliasKey];
  if (aliasId) {
    return {
      kind: "url",
      value: testvideoUrl(aliasId),
      label: TestvideoMeta[aliasId] ?? aliasId,
      raw,
    };
  }

  // Bare testvideo stem (or stem.mp4).
  const stem = raw.replace(/\.mp4$/i, "");
  const known = Object.values(TestvideoClip).find((id) => id === stem);
  return {
    kind: "url",
    value: testvideoUrl(stem),
    label: known ? TestvideoMeta[known] : `${stem}.mp4`,
    raw,
  };
}
