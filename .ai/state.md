# State

> In progress + Next up (+ Later) only. No Done list — finished items are deleted; durable lessons go to skills / product / architecture (`AGENTS.md` Documentation).

---

## In progress

### Handoff (2026-10-08 → next chat)

- **Branch:** `main`, ahead of `origin` (through **3.2**). Do not push unless asked.
- **Group 3 landed:**
  - 3.1 — play session in `VideoPlayer` (`10869fc`).
  - 3.2 — Seek/Step via transport FSM; UI `playing` ← `TransportState::ui_playing`; SoftPause always `pause_for_scrub`. Cite: `mediabunny: examples/media-player/media-player.ts` `seekToTime`.
  - Loop seam: `ResetForPlay` → HoldScrub across `loop_to_start`, warm-prefetch before resume (eof-loop `holdsB` 0). See `skills/media.md` triage.
- **e2e:** full suite green (38 passed) after 3.2.
- **Dirty (leave):** `e2e/baselines/pre-decode-sink/avc_1080p_24_gop{12,48}_bf0.json` — probe noise; `e2e-*.log` untracked.
- **Invariants:** engine `Shared.playing` still a separate `Cell`; approve-before-edit + mediabunny cite for media.
- **Next chat starts at:** group 3.3 — drop `thread_local!` coalescing (`app::defer_scrub_pointer`, `hover_thumb.rs`).

---

## Next up

Refactor plan from the 2026-10-08 codebase review. One bullet ≈ one PR; keep order. Group 3 → `AGENTS.md` Pre-edit gate (mediabunny cite) per PR. Update the matching `.ai/` row in the same PR (not ahead of code).

### 3. Behavior-adjacent (mediabunny cite required)

1. Drop `thread_local!` coalescing (`app::defer_scrub_pointer`, `hover_thumb.rs`) — breaks the 1–2 source invariant. `hover_thumb` defer → `leptos-use` `use_throttle_fn` (trailing) in `HoverThumbPreview` scope.

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
