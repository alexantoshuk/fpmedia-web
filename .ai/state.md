# State

> In progress + Next up (+ Later) only. No Done list — finished items are deleted; durable lessons go to skills / product / architecture (`AGENTS.md` Documentation).

---

## Handoff (2026-10-04)

**Docs:** `AGENTS.md` documentation write gate + skill budgets. Canonical cleanup: `product` / `architecture` / `state` / `skills/*` (no code changes this handoff).

**Engine (prior, on `main`):** short B-frame `flush/idle` after ChG park — `saw_target` clear, `pts_aim_hole`, cascade through aim, `prune_floor_us`. Main play still probes `prefer-hardware` (Hd+Uhd); scrub/thumb never steal HW. Gotchas → `.ai/skills/media.md`.

---

## Next up

### Docs (if needed after review)

1. Spot-check skills against write-gate budgets after real media fixes.

### Play / decode

1. Optional: relax ChG series `held_min` → `pres_min`, or chase residual fps_min 23.5.

### Deferred

1. Play hover thumb: if 800 ms floor + 150 ms UI defer still janks → try 1000 ms or Play cache-only (no aim decode).

### Also open

1. Drop `re_mp4` fork when upstream has #35 + #37.
2. Native `FileSource`; no-Range `ReadableStreamSource`; optional `hevc` defaulting later.

### Later

- Tab-hidden `AudioContinue`; pure `AudioPlayer`; thumb Worker; 4K loop residual; reverse playback (if revisited); dual-source; publish crates; production chrome.
