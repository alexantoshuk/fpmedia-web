# State

> In progress + Next up (+ Later) only. No Done list — finished items are deleted; durable lessons go to skills / product / architecture (`AGENTS.md` Documentation).

---

## In progress

### Handoff (2026-10-08 chat → next)

- **Branch:** `main`, ahead of `origin` (local commits through `7c67ef5`). Do not push unless asked.
- **Groups 1–2 (structural):** landed. Layout now `media/src/model/` · `policy/` · `host/`; `residency::AimWindow`; typed `MediaError::DecodeStall` / `DecoderPoison`; one `open_mp4`; `webmedia::timer`; Player folded into `Shared`; strip math in `media_components::strip`.
- **Group 3 started (behavior-adjacent):**
  - `3f165ea` — `apply_engine_event` runs `EngineCmd` (soft `EnterScrub` = no densify; `EndExact { densify }`).
  - `7c67ef5` — `model::bg_task::SingleFlight` ×3 (densify / gray / thumb); **not** one merged task.
- **e2e:** full Playwright green **after groups 1–2 only** (37 passed, 1 skipped). **Not re-run** after EngineCmd / SingleFlight — re-run before trusting play-session work.
- **Node:** portable LTS at `%LOCALAPPDATA%\Programs\node-v24.21.0-win-x64` (user PATH). Agent shells may still prefer Zed’s older node — prepend that dir. `winget` absent on this box.
- **Dirty (leave):** `e2e/baselines/pre-decode-sink/avc_1080p_24_gop{12,48}_bf0.json` — pre-existing / probe noise; not part of refactor commits.
- **Invariants still true:** `Shared.playing` is a separate `Cell` (`EngineMode` can be ExactHold/Scrub mid-play); approve-before-edit + mediabunny cite for media; no `.cursor/` docs tree.
- **Next chat starts at:** group 3.1 below (play session → `VideoPlayer`). Cite: `mediabunny: examples/media-player/media-player.ts` render / play / pause.

---

## Next up

Refactor plan from the 2026-10-08 codebase review. One bullet ≈ one PR; keep order. Group 3 → `AGENTS.md` Pre-edit gate (mediabunny cite) per PR. Update the matching `.ai/` row in the same PR (not ahead of code).

### 3. Behavior-adjacent (mediabunny cite required)

1. Move the play session into `VideoPlayer` (prefetch loop, underrun hold/resume, loop, visibility, silent-clock arm); drop host `PlaySession` flags. `mediabunny: examples/media-player/media-player.ts` render / play / pause.
2. Seek / Step through the transport FSM; derive `playing` from transport (engine `Shared.playing` is a separate `Cell` today — `EngineMode` can be `ExactHold` / `Scrub` mid-play); rewrite `skills/leptos.md` “`playing` is UI-owned”.
3. Drop `thread_local!` coalescing (`app::defer_scrub_pointer`, `hover_thumb.rs`) — breaks the 1–2 source invariant. `hover_thumb` defer → `leptos-use` `use_throttle_fn` (trailing) in `HoverThumbPreview` scope.

### Play / decode

1. Optional: ChG-class play residual fps_min ≈ 23.5 — idea “relax series `held_min` → `pres_min`”. Metrics are not defined in code / e2e: write the probe first.

---

## Later

- Re-run `npm run e2e` after group 3.1–3.2 (EngineCmd + SingleFlight) before merging further play/transport work.
- Classify browser codec faults by DOMException `name` (`EncodingError` / `InvalidStateError` / `DataError`) instead of message text in `MediaError::from_codec_message` — behavior change (per-browser wording).
- `ThumbCache` on `residency::AimWindow` (aim-distance instead of GOP LRU) — behavior change; probe hover first.
- Play hover thumb: if 800 ms floor + 150 ms UI defer still janks → try 1000 ms or Play cache-only (no aim decode).
- Drop `re_mp4` fork when upstream has #35 + #37.
- Native `FileSource`; no-Range `ReadableStreamSource`; optional `hevc` defaulting.
- Tab-hidden `AudioContinue`; pure `<Audio>` / `AudioPlayer` (reuses `webmedia::audio`); thumb Worker; 4K loop residual; reverse playback (if revisited); dual-source; publish crates; production chrome.
