# State

> In progress + Next up (+ Later) only. No Done list — finished items are deleted; durable lessons go to skills / product / architecture (`AGENTS.md` Documentation).

---

## Handoff (2026-10-03)

**Landed (uncommitted):** short B-frame `flush/idle` after ChG — `saw_target` clear on retarget, `pts_aim_hole`, cascade through aim, `prune_floor_us` + consumer prune at spawn aim, Hd AVC no auto-prefer HW. Verified after Chrome hard-refresh. Gotcha → `.ai/skills/media.md`.

---

## Next up

### Play / decode

1. Optional: relax ChG series `held_min` → `pres_min`, or chase residual fps_min 23.5.

### Deferred

1. Play hover thumb: if 800 ms floor + 150 ms UI defer still janks → try 1000 ms or Play cache-only (no aim decode).

### Also open

1. Drop `re_mp4` fork when upstream has #35 + #37.
2. Native `FileSource`; no-Range `ReadableStreamSource`; optional `hevc`.

### Later

- Tab-hidden `AudioContinue`; pure `AudioPlayer`; thumb Worker; 4K loop residual; dual-source; publish crates; production chrome.
