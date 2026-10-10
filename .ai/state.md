# State

> In progress + Next up (+ Later) only. No Done list — finished items are deleted; durable lessons go to skills / product / architecture (`AGENTS.md` Documentation).

---

## In progress

### Handoff

- **Branch:** `main`, ahead of `origin`. Do not push unless asked.
- **Group 3:** 3.1–3.3 verified — e2e **38 passed** (~5.5m, baseline compare) after TLS coalesce drop.
- **Dirty (leave):** `e2e/baselines/pre-decode-sink/avc_1080p_24_gop{12,48}_bf0.json`.
- **Silent clock:** `PlaybackClock` advances on wall time (Chrome `currentTime` lag under decode load). ChG original residual ≈ AAC remux (realtime≈1.0, fps_min≈22 / p05≈23 / p50≈25). Uncommitted: `playback_clock.rs` + `ConstantSourceNode` feature + probe/docs.

---

## Next up

### Play / decode

1. Optional: remaining fps_min≈22–23 floor on ChG (AAC or silent+wall) — not skip path; thin held + full pres is steady-state. Or park residual as known / Later.
2. Commit silent-clock fix when asked.

---

## Later

- Classify browser codec faults by DOMException `name` (`EncodingError` / `InvalidStateError` / `DataError`) instead of message text in `MediaError::from_codec_message` — behavior change (per-browser wording).
- `ThumbCache` on `residency::AimWindow` (aim-distance instead of GOP LRU) — behavior change; probe hover first.
- Play hover thumb: if 800 ms floor + 150 ms UI defer still janks → try 1000 ms or Play cache-only (no aim decode).
- Drop `re_mp4` fork when upstream has #35 + #37.
- Native `FileSource`; no-Range `ReadableStreamSource`; optional `hevc` defaulting.
- Tab-hidden `AudioContinue`; pure `<Audio>` / `AudioPlayer` (reuses `webmedia::audio`); thumb Worker; 4K loop residual; reverse playback (if revisited); dual-source; publish crates; production chrome.
