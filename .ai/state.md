# State

> In progress + Next up (+ Later) only. No Done list — finished items are deleted; durable lessons go to skills / product / architecture (`AGENTS.md` Documentation).

---

## In progress

### Handoff

- **Branch:** `main` (ahead of origin) — densify hole-fill + Exact orphan farthest-evict on densify ticks; soft-drag Exact preview kept.
- **Verified (ChG):** Pause drops Exact 1 px past holes; scrub warm trail intact.
- **Do not:** prune Exact on every drag `set_playhead`; soft-drag preview from Covered nearest.

---

## Next up

_(empty — strip densify / Exact-orphan UX closed; pick from Later or a new symptom)_

---

## Later

- Optional: unthrottle play `sync_ui` for truer e2e playhead sampling (host only; not an engine judder fix).
- Classify browser codec faults by DOMException `name` (`EncodingError` / `InvalidStateError` / `DataError`) instead of message text in `MediaError::from_codec_message` — behavior change (per-browser wording).
- `ThumbCache` on `residency::AimWindow` (aim-distance instead of GOP LRU) — behavior change; probe hover first.
- Play hover thumb: if 800 ms floor + 150 ms UI defer still janks → try 1000 ms or Play cache-only (no aim decode).
- Switch `re_mp4` from git pin to crates.io when a release includes #35 + #38.
- Native `FileSource`; no-Range `ReadableStreamSource`; optional `hevc` defaulting.
- Tab-hidden `AudioContinue`; pure `<Audio>` / `AudioPlayer` (reuses `webmedia::audio`); thumb Worker; 4K loop residual; reverse playback (if revisited); dual-source; publish crates; production player UI.
