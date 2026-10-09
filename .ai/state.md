# State

> In progress + Next up (+ Later) only. No Done list — finished items are deleted; durable lessons go to skills / product / architecture (`AGENTS.md` Documentation).

---

## In progress

### Handoff

- **Branch:** `main`, ahead of `origin`. Do not push unless asked.
- **Group 3:** 3.1–3.3 landed (play session, transport Seek/Step, drop TLS coalesce). Refactor plan groups empty except optional Play/decode.
- **e2e:** re-run after 3.3 (TLS drop) before treating group 3 fully verified.
- **Dirty (leave):** `e2e/baselines/pre-decode-sink/avc_1080p_24_gop{12,48}_bf0.json`.
- **Next:** optional Play/decode ChG residual probe, or Later backlog.

---

## Next up

### Play / decode

1. Optional: ChG-class play residual fps_min ≈ 23.5 — idea “relax series `held_min` → `pres_min`”. Metrics are not defined in code / e2e: write the probe first.

---

## Later

- Classify browser codec faults by DOMException `name` (`EncodingError` / `InvalidStateError` / `DataError`) instead of message text in `MediaError::from_codec_message` — behavior change (per-browser wording).
- `ThumbCache` on `residency::AimWindow` (aim-distance instead of GOP LRU) — behavior change; probe hover first.
- Play hover thumb: if 800 ms floor + 150 ms UI defer still janks → try 1000 ms or Play cache-only (no aim decode).
- Drop `re_mp4` fork when upstream has #35 + #37.
- Native `FileSource`; no-Range `ReadableStreamSource`; optional `hevc` defaulting.
- Tab-hidden `AudioContinue`; pure `<Audio>` / `AudioPlayer` (reuses `webmedia::audio`); thumb Worker; 4K loop residual; reverse playback (if revisited); dual-source; publish crates; production chrome.
