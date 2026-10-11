# Product

> Stable domain and UX context. Read at start of every session.
> Change only after a product decision proves itself across sessions.
> Implementation traps → `.ai/skills/`; stack/design → `.ai/architecture.md`.

---

## What

**webmedia** — frame-accurate media engine + HTML-media-like elements for precise frame-by-frame review in the browser.

**Primary deliverable:** low-level components (`<Video>`, later `<Audio>`) and engine API — not a shipped player chrome. Alpha: open an MP4, seek/step/scrub by presentation frame, play/pause via WebCodecs → canvas. Not a generic `<video>` wrapper — a Rust/Leptos analog with frame-primary control.

**References**:
- UX: Frame.io review player (precision scrub **behavior** — not a target UI to ship in the element crate).
- Implementation: **[mediabunny](https://github.com/Vanilagy/mediabunny)** — adapt patterns into Rust; do **not** add it as a dependency. How: `AGENTS.md` Mediabunny-first; traps: `.ai/skills/media.md`. Mediabunny ships sinks + examples (no scrubber UI in the library); our element helpers are an extra Leptos layer over `webmedia`.

---

## Surfaces

| Surface | Purpose |
| ------- | ------- |
| **`webmedia` crate** | Engine — demux + frame index + `VideoPlayer`; audio under `audio/`. Using only this + hand-rolled demo UI ≈ mediabunny’s example style. |
| **`media_components` crate** | Element analogs + headless track helpers (below) — hide engine/transport/`pipeline` from host UI authors. **No** player chrome. |
| **Demo app** | `app` / `frontend` — **all** player UI: file picker, transport buttons, keyboard (JKL/Space/arrows), progress look, ticks, strip layout/colors, thumb chrome, status. Not production player UI. |

### Element-layer helpers (shipped)

Headless / paint-inside components in `media_components` (compose in demo; chrome stays in demo):

| Piece | Role |
| ----- | ---- |
| `<Video>` / `VideoRef` | Stage + element API (open/play/seek/rate/volume). |
| `<Scrubber>` | Gesture + track container + context. Owns pointer → playhead + `scrub_pointer_*` / transport / scrub phases. Host: children + CSS; may observe `scrubbing`. Does **not** ship progress fill, ticks, or strip layout. |
| `<TimelineMeta kind>` | One timeline meta strip (`Fetch` / `Covered` / `Exact`). Hides `pipeline` → px-runs → 1 px canvas paint. Host: `class` / color + DOM placement (overlay vs stacked rows). |
| `<HoverThumb>` | Hover preview: canvas **inside**; props `width` / `height` / `class`; `children` = chrome (border, timecode, frame #); exposes `frame` / `visible`. Position (`transform`) computed from scrubber `update(x, track_w, frame)`. Auto-wire when under `<Scrubber>`; manual API escape hatch OK. |

`video.pipeline` may remain for a **debug** panel — not required for scrubber UI. Math helpers (`playhead_width_*`, `scrub_frame_from_x`, `strip::*`) stay for custom chrome. Phases design: `architecture.md` § Scrub phases.

---

## Invariants

- **Primary position** = presentation `FrameId` (0-based), not `time * fps`.
- **Exact vs Play (agreed):** “Frame-accurate” means **seek / step / scrub settle / Pause** — Exact decode to the aimed frame; paused canvas is that frame. **Continuous Play prioritizes smoothness** (cadence, A/V sync, no micro-stutter) over painting every presentation frame. Do **not** invent Play-only “show every frame” / soft-pace pipelines that fight realtime. Shuttle (≥2×) may drop overdue frames. Prefer mediabunny media-player present+clock patterns.
- Exact frame N uses keyframe cascade (nearest sync → decode → target), including B-frame / decode-order windows — for **seek / settle / step / Pause**, not as a Play smoothness tax.
- Layout/state must allow **1–2 media sources** later (compare) — never hard-assume a single source forever.
- `<Video>` stage lays out like a sized `<video>`: fixed viewport, **contain**-fit picture, metadata via the element API.
- Continuous play keeps **A/V sync** on an audio-driven clock — also for clips without an audio track. The element API exposes volume / mute; it does not expose the clock. Clock design: `architecture.md` § Audio + media clock.
- **Audio codecs (agreed):** AAC (`mp4a`) and common QT/ISO **PCM** (`in24` / `sowt` / `twos` / …) play through the same volume/mute/rate path; PCM does not use WebCodecs `AudioDecoder`. Unsupported audio sample entries are ignored (video still opens).
- **Elements first, UI in demo only (agreed):** primary product = element + headless helpers above. Player chrome (buttons, progress look, ticks, strip composition, thumb labels/borders, keyboard) lives **only** in the host/demo. Do **not** grow production-styled player UI inside `media_components`. Flag-soup props (`show_progress`, `strip_layout`) on one mega-scrubber are out — compose instead.
- **Hover thumbs:** Frame.io-style timeline preview. **Agreed UX:** Play **and** Pause, not while scrubbing; must not drive densify / buffer strips. Chrome around the paint surface = host (`<HoverThumb>` children / `class`).
- **Play first, scrub second:** smooth **1× play** must work for any supported clip (incl. 4K). Exactness is Pause/seek/step — not a reason to slow Play. Scrub/densify yields while playing. Do not retune Full-HD play defaults when tuning 4K.
- **Play vs scrub residency (agreed):** Play stays light — no background scrub caching while playing. Timeline meta kinds: **Fetch** (gray / fetched bytes), **Covered** (dim green / decoded coverage), **Exact** (bright green / step-ready); demo owns colors and whether strips overlay or stack. Strips stay crisp (no blur) on long clips. Green paint mirrors residency (small holes OK while densify fills — no soft-merge / min-px inflate). No separate buffer meter bars; live decode numbers live in the demo pipeline panel. Next/Prev near the playhead are instant when step-ready. Resize/fullscreen keeps the strips. Design: `architecture.md` § Play / scrub residency.
- **Playback rate:** review slider `0.25…1.75` (demo); API up to **8×**. Demo **JKL** (forward only): `1, 2, 4, 8` — **J** slower, **L** faster, **K** → **1×** + play/pause (**Space** toggles without resetting rate). Fixed-pitch audio through **4×**; **8×** silent. Reverse playback removed (open work: `state.md` Later if revisited).

---

## Non-goals (for now)

- Full FFmpeg-in-browser convert/mux toolkit (mediabunny’s full scope).
- Multi-container write path; focus read + decode of MP4 (AVC; **HEVC** opt-in via cargo features — off by default).
- Production auth, storage, or collaboration UI (lives in host apps).
- Production player UI — demo controls in `app` are temporary.
- Production CORS / open media proxy — demo `/proxy` is localhost-only (local tests / e2e).
