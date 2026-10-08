# State

> In progress + Next up (+ Later) only. No Done list — finished items are deleted; durable lessons go to skills / product / architecture (`AGENTS.md` Documentation).

---

## In progress

### Handoff (2026-10-08 → next)

- **Branch:** `main`, ahead of `origin` (local commits through prior group-3 work + uncommitted **3.1 play session → VideoPlayer**). Do not push unless asked.
- **Groups 1–2 (structural):** landed earlier.
- **Group 3:**
  - EngineCmd + SingleFlight (prior commits).
  - **3.1 landed (this session, uncommitted):** play session in `VideoPlayer` — `begin_play_session` / `play_raf_tick` / prefetch driver / underrun / loop seam / silent-clock; host `PlaySession` removed. Cite: `mediabunny: examples/media-player/media-player.ts` render / play / pause.
- **e2e:** full Playwright green after 3.1 (**38 passed**). Baselines JSON rewritten by probe — leave out of refactor commit.
- **Dirty (leave):** `e2e/baselines/pre-decode-sink/avc_1080p_24_gop{12,48}_bf0.json` — probe noise.
- **Invariants:** `Shared.playing` still a separate `Cell`; UI `playing` still host-owned until 3.2; approve-before-edit + mediabunny cite for media.
- **Next chat starts at:** group 3.2 — Seek / Step through transport FSM; derive `playing` from transport.

---

## Next up

Refactor plan from the 2026-10-08 codebase review. One bullet ≈ one PR; keep order. Group 3 → `AGENTS.md` Pre-edit gate (mediabunny cite) per PR. Update the matching `.ai/` row in the same PR (not ahead of code).

### 3. Behavior-adjacent (mediabunny cite required)

1. Seek / Step through the transport FSM; derive `playing` from transport (engine `Shared.playing` is a separate `Cell` today — `EngineMode` can be `ExactHold` / `Scrub` mid-play); rewrite `skills/leptos.md` “`playing` is UI-owned”.
2. Drop `thread_local!` coalescing (`app::defer_scrub_pointer`, `hover_thumb.rs`) — breaks the 1–2 source invariant. `hover_thumb` defer → `leptos-use` `use_throttle_fn` (trailing) in `HoverThumbPreview` scope.

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
