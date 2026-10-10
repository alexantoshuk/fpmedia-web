# State

> In progress + Next up (+ Later) only. No Done list — finished items are deleted; durable lessons go to skills / product / architecture (`AGENTS.md` Documentation).

---

## In progress

### Handoff

- **Branch:** `main` — densify/strip fixes; soft-drag preview from Exact blit; Exact far-prune reverted (killed scrub warm trail).
- **Verify:** rebuild WASM + hard-reload ChG — drag canvas responsive again; Covered fills; Exact 1 px trail ticks accepted (warm blit).
- **Do not:** prune Exact on aim/settle (scrub regress); soft-drag preview from Covered nearest.

---

## Next up

- Strip UX (Pause densify OK; scrub must stay intact):
  - Covered / Exact strip **1–2 px** flicker / snap noise on long clips.
  - **Dim green** can lag behind scrub feel (Covered paint slower than pointer / blit) — often scrub Exact trail vs Pause PreferSoftware densify, not a paint bug; Pause densify speed not priority.

---

## Later

- Optional: unthrottle play `sync_ui` for truer e2e playhead sampling (host only; not an engine judder fix).
- Classify browser codec faults by DOMException `name` (`EncodingError` / `InvalidStateError` / `DataError`) instead of message text in `MediaError::from_codec_message` — behavior change (per-browser wording).
- `ThumbCache` on `residency::AimWindow` (aim-distance instead of GOP LRU) — behavior change; probe hover first.
- Play hover thumb: if 800 ms floor + 150 ms UI defer still janks → try 1000 ms or Play cache-only (no aim decode).
- Switch `re_mp4` from git pin to crates.io when a release includes #35 + #38.
- Native `FileSource`; no-Range `ReadableStreamSource`; optional `hevc` defaulting.
- Tab-hidden `AudioContinue`; pure `<Audio>` / `AudioPlayer` (reuses `webmedia::audio`); thumb Worker; 4K loop residual; reverse playback (if revisited); dual-source; publish crates; production player UI.
