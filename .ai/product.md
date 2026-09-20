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
| **`media_components` crate** | Leptos `<Video>` + `VideoRef` — media element API only (HTML `<video>` analog). Modules: `video`, `helpers` (+ planned `audio`) |
| **Demo app** | `app` / `frontend` — file picker, transport buttons, **keyboard (JKL/Space/arrows)**, status (not production chrome) |
| **Library API** | `webmedia` crate — demux + frame index + `Player` / `VideoPlayer`; audio engine under `audio/` |

---

## Invariants

- **Primary position** = presentation `FrameId` (0-based), not `time * fps`.
- Exact frame N uses keyframe cascade (nearest sync → decode → target), including B-frame / decode-order windows.
- Player layout/state must allow **1–2 media sources** later (compare) — never hard-assume a single source forever.
- `<Video>` stage lays out like a sized `<video>`: fixed viewport, **contain** canvas, source metadata via `VideoRef::metadata` / `VideoPlayer::metadata()`.
- Continuous play uses a **wall-clock** media timeline until an audio track exists; then video slaves to audio
  (`AudioContext` clock — mediabunny media-player pattern).
- Audio decode/output lives in the **`webmedia` engine** (`audio/` module), not inside `VideoRef`. UI hosts stay thin
  (volume/mute/transport **API** only — no keyboard bindings). A future pure-**`<Audio>` / `AudioRef`** reuses the same
  engine modules without a video track or canvas.
- **Component vs demo:** media element crates expose **API only** (like HTML `<video>` / `<audio>`). Keyboard shortcuts,
  progress-bar chrome math for caches live in **`media_components::helpers`**; demo transport /
  keyboard stay in **`app`**.
- **Play first, scrub second:** smooth **1× play with audio** must work for any supported clip (incl. 4K via
  `PlayPolicy`). Scrub/densify yields while playing (`CacheFillMode::Play`). Full-HD play+scrub knobs stay on
  the historical Hd defaults — do not retune them when adding Uhd play ahead/HW.
- **Play vs scrub residency (agreed):** Play stays mediabunny-light (tiny `VideoFrame` pool, densify off). Scrub densify critical path = **decode → `ImageBitmap` → canvas** (`mediabunny:` CanvasSink-style). `ScrubChunkStore` holds **resident markers** only (gap geometry). Demo strip: dim green = marker coverage, bright green = blit window (two CSS gradient overlays; kept on Play for markers). **Seek / settle** paint decode `VideoFrame`. **Next / Prev** prefer VF ring, else warm blit, else soft warm decode (densify keeps running). Resize/fullscreen keeps scrub trail; canvas buffer wipe → repaint from `displayed` (else warm blit).
- **Playback rate:** review slider `0.25…1.75` (demo UI); API allows up to **8×**. Demo **JKL** (forward only): ladder `1, 2, 4, 8` — **J** slower, **L** faster, **K** → **1×** + play/pause (unlike **Space**, which toggles without resetting rate). Fixed-pitch audio through **4×** (WSOLA ≈ HTML `preservesPitch`); **8×** silent. Shuttle present policy still starts at **2×**. **Reverse playback removed** (notes in `skills/media.md`).

---

## Non-goals (for now)

- Full FFmpeg-in-browser convert/mux toolkit (mediabunny’s full scope).
- Multi-container write path; focus read + decode of MP4 (AVC + HEVC bitstream key-verify).
- Production auth, storage, or collaboration UI (lives in host apps).
- Production player chrome — demo controls in `app` are temporary.
