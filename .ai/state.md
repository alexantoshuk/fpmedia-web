# State

> In progress + Next up (+ Later) only. No Done list — finished items are deleted; durable lessons go to skills / product / architecture (`AGENTS.md` Documentation).

---

## Handoff (2026-10-04)

**Goal of the VideoPlayer split (shipped):** navigation-only extracts — **no playback behavior change**. Empty `video_player/mod.rs` into focused modules; host API stays what `media_components` calls.

**Shipped on `main` (P0–P6 + API pass):**  
`shared` · `util` · `scrub_plan` (+ wasm `scrub_gap` / `scrub_cold`) · `clock_ctrl` · `present` · `decode_ops` · `open` · `control` · `api`. `mod.rs` = thin façade. Sibling-only helpers → `pub(super)`; `PRESENTATION_QUEUE_CAP` / `STEP_BEHIND_CAP` not re-exported from `lib.rs`. Module map + host-vs-engine note → `.ai/skills/media.md`.

**Agent workflow:** write gate / skill budgets; portable docs (no `.cursor/` tree); **ask-before-commit** — edit/`go` ≠ commit consent (`AGENTS.md` § Commits). Agent does **not** force-push `main`.

**Checks:** wasm player code needs `--features webcodecs` (or check via `media_components`). Prefer `cargo check` while iterating; Clippy `-D warnings` only at commit time (`CONTRIBUTING.md`).

**Engine (prior, unchanged by the split):** short B-frame `flush/idle` after ChG park — `saw_target` / `pts_aim_hole` / cascade / `prune_floor_us`. Main play `prefer-hardware` (Hd+Uhd); scrub/thumb never steal HW. Gotchas → `.ai/skills/media.md`.

**If local `main` still diverged after commit-message amend:** `git push --force-with-lease origin main` (human only).

---

## Next up

### Maintainability (refactor remainder)

1. Optional navigation-only splits (same rules as P0–P6): `scrub.rs` (~1.6k lines) then `play.rs` (~0.9k). No behavior change; cite mediabunny only if a fix sneaks in.

### Play / decode

1. Optional: relax ChG series `held_min` → `pres_min`, or chase residual fps_min 23.5.

### Deferred

1. Play hover thumb: if 800 ms floor + 150 ms UI defer still janks → try 1000 ms or Play cache-only (no aim decode).

### Also open

1. Drop `re_mp4` fork when upstream has #35 + #37.
2. Native `FileSource`; no-Range `ReadableStreamSource`; optional `hevc` defaulting later.

### Later

- Tab-hidden `AudioContinue`; pure `AudioPlayer`; thumb Worker; 4K loop residual; reverse playback (if revisited); dual-source; publish crates; production chrome.
