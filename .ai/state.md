# State

> In progress + Next up (+ Later) only. No Done list — finished items are deleted; durable lessons go to skills / product / architecture (`AGENTS.md` Documentation).

---

## In progress

- Refactor plan below: group 3 next (e2e green after groups 1–2: 37 passed, 1 skipped).

---

## Next up

Refactor plan from the 2026-10-08 codebase review. One bullet ≈ one PR; keep order. Groups 1–2 = no behavior change. Group 3 changes behavior → `AGENTS.md` Pre-edit gate (mediabunny cite) per PR. Update the matching `.ai/` row in the same PR (not ahead of code).

### 3. Behavior-adjacent (mediabunny cite required)

1. Apply `EngineCmd` from `engine_reduce` (8 sites discard it) instead of hand-coded side effects.
2. Single-flight background task for densify / gray fill / thumb fill (replaces `scrub_busy` / `scrub_pending` / `gray_fill_busy` latches).
3. Move the play session into `VideoPlayer` (prefetch loop, underrun hold/resume, loop, visibility, silent-clock arm); drop host `PlaySession` flags. `mediabunny: examples/media-player/media-player.ts` render / play / pause.
4. Seek / Step through the transport FSM; derive `playing` from transport (engine `Shared.playing` is a separate `Cell` today — `EngineMode` can be `ExactHold` / `Scrub` mid-play); rewrite `skills/leptos.md` “`playing` is UI-owned”.
5. Drop `thread_local!` coalescing (`app::defer_scrub_pointer`, `hover_thumb.rs`) — breaks the 1–2 source invariant. `hover_thumb` defer → `leptos-use` `use_throttle_fn` (trailing) in `HoverThumbPreview` scope.

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
