# Product

> Stable domain and UX context. Read at start of every session.
> Change only after a product decision proves itself across sessions.

---

## What

**webmedia** — frame-accurate custom web media player for precise frame-by-frame review in the browser.

Alpha goal: open an MP4, seek/step/scrub by presentation frame, play/pause via WebCodecs → canvas. Not a generic `<video>` wrapper — a Rust/Leptos analog with frame-primary control.

**References**:
- UX: Frame.io review player (precision scrub).
- Implementation: **[mediabunny](https://github.com/Vanilagy/mediabunny)** is the standing reference — adapt patterns into Rust; do **not** add it as a dependency. How: `AGENTS.md` Mediabunny-first; traps: `.ai/skills/media.md`.

---

## Surfaces

| Surface | Purpose |
| ------- | ------- |
| **`video` crate** | Leptos `<Video>` + `VideoRef` — media element (canvas, open/play/seek/step/scrub); HTML `<video>` analog |
| **Demo app** | `app` / `frontend` — file picker, transport buttons, status (not production chrome) |
| **Library API** | `webmedia` crate — demux + frame index + `Player` / `WebPlayer` |

---

## Invariants

- **Primary position** = presentation `FrameId` (0-based), not `time * fps`.
- Exact frame N uses keyframe cascade (nearest sync → decode → target), including B-frame / decode-order windows.
- Player layout/state must allow **1–2 media sources** later (compare) — never hard-assume a single source forever.
- `<Video>` stage lays out like a sized `<video>`: fixed viewport, **contain** canvas, source metadata via `VideoRef::metadata` / `WebPlayer::metadata()`.
- Continuous play uses a **wall-clock** media timeline until an audio track exists; then video slaves to audio
  (`AudioContext` clock — mediabunny media-player pattern).
- Audio decode/output lives in the **`webmedia` engine**, not inside `VideoRef`. UI hosts stay thin
  (volume/mute/transport). A future pure-**`<Audio>` / `AudioRef`** reuses the same demux + decode + clock
  without a video track or canvas.
- **Play vs scrub residency (agreed):** Play stays mediabunny-light (tiny `VideoFrame` pool, densify off). Scrub densify critical path = **decode → `ImageBitmap` → canvas** (`mediabunny:` CanvasSink-style). `ScrubChunkStore` holds **resident markers** only (gap geometry). Demo strip: dim green = marker coverage, bright green = blit window (two CSS gradient overlays; kept on Play for markers). **Seek / settle** paint decode `VideoFrame`. **Next / Prev** prefer VF ring, else warm blit, else soft warm decode (densify keeps running). Resize/fullscreen keeps scrub trail; canvas buffer wipe → repaint from `displayed` (else warm blit).
- **JKL shuttle (Frame.io):** ladder `−8, −4, −2, −1, 1, 2, 4, 8`. **J** walks left, **L** walks right (clamp at ends; path through **±1×** before flipping direction). **K** resets rate to **1×**, then pause/play. Demo keys: `app/src/player.rs` (`JKL_STEPS`). Review slider `0.25…1.75` is separate from the ladder.

---

## Non-goals (for now)

- Full FFmpeg-in-browser convert/mux toolkit (mediabunny’s full scope).
- Multi-container write path; focus read + decode of MP4/H.264 first (HEVC key-verify still open).
- Production auth, storage, or collaboration UI (lives in host apps).
- Production player chrome — demo controls in `app` are temporary.
