# State

> In progress + Next up (+ Later) only. No Done list — finished items are deleted; durable lessons go to skills / product / architecture (`AGENTS.md` Documentation).

---

## In progress

- Refactor plan below (group 1).

---

## Next up

Refactor plan from the 2026-10-08 codebase review. One bullet ≈ one PR; keep order. Groups 1–2 = no behavior change. Group 3 changes behavior → `AGENTS.md` Pre-edit gate (mediabunny cite) per PR. Update the matching `.ai/` row in the same PR (not ahead of code).

### 1. Hygiene (no behavior change)

1. `media_components`: one `HostCtx` instead of 10–16-arg `run_open` / `dispatch_transport`; one reset for `close_fn` + `retire_clip`; `VideoRef` ops boilerplate.
2. Split hover thumbs out of `video_player/scrub.rs` → `thumb.rs` (module-level `cfg(feature = "thumbs")`, no inline stubs).
3. Split `types.rs` (policy / metadata); move strip pixel math to `media_components::helpers`.
4. One JS timer module (sleep / macrotask / microtask / rAF / now) for engine + host; removes bare-`resolve` timers (`video_player/util.rs::sleep_ms`, `hover_thumb.rs`) and `expect("window")`.

### 2. Structural, low risk

1. Collapse `Player` into `Shared` (`Cell` frame / looping; `playing` from `EngineMode`); no `metadata().clone()` on hot paths.
2. Shared aim-distance residency for `ScrubChunkStore` / `ScrubBlitLru` / `ThumbCache`.
3. Typed decode faults instead of matching error strings (`decode_ops.rs`, `util::is_decoder_poison`).
4. One `open_mp4(source, opts, progress)` entry instead of 8 `open_mp4_*` variants.
5. Group `webmedia` root modules (`model/` · `host/` · `policy/`).

### 3. Behavior-adjacent (mediabunny cite required)

1. Apply `EngineCmd` from `engine_reduce` (8 sites discard it) instead of hand-coded side effects.
2. Single-flight background task for densify / gray fill / thumb fill (replaces `scrub_busy` / `scrub_pending` / `gray_fill_busy` latches).
3. Move the play session into `VideoPlayer` (prefetch loop, underrun hold/resume, loop, visibility, silent-clock arm); drop host `PlaySession` flags. `mediabunny: examples/media-player/media-player.ts` render / play / pause.
4. Seek / Step through the transport FSM; derive `playing` from transport; rewrite `skills/leptos.md` “`playing` is UI-owned”.
5. Drop `thread_local!` coalescing (`app::defer_scrub_pointer`, `hover_thumb.rs`) — breaks the 1–2 source invariant.

### Play / decode

1. Optional: ChG-class play residual fps_min ≈ 23.5 — idea “relax series `held_min` → `pres_min`”. Metrics are not defined in code / e2e: write the probe first.

---

## Later

- Play hover thumb: if 800 ms floor + 150 ms UI defer still janks → try 1000 ms or Play cache-only (no aim decode).
- Drop `re_mp4` fork when upstream has #35 + #37.
- Native `FileSource`; no-Range `ReadableStreamSource`; optional `hevc` defaulting.
- Tab-hidden `AudioContinue`; pure `<Audio>` / `AudioPlayer` (reuses `webmedia::audio`); thumb Worker; 4K loop residual; reverse playback (if revisited); dual-source; publish crates; production chrome.
