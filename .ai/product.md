# Product

> Stable domain and UX context. Read at start of every session.
> Change only after a product decision proves itself across sessions.
> Implementation traps → `.ai/skills/`; stack/design → `.ai/architecture.md`.
> Shared vocabulary (settle, densify, Exact, Covered, …) → `.ai/glossary.md`.

---

## What

**fpmedia-web** — frame-precise media engine + HTML-media-like elements for precise frame-by-frame review in the browser.

**Primary deliverable:** low-level components (`<Video>`, later `<Audio>`) and engine API — not a shipped player chrome. Alpha: open an MP4, seek/step/scrub by presentation frame, play/pause via WebCodecs → canvas. Not a generic `<video>` wrapper — a Rust/Leptos analog with frame-primary control.

**References**:
- UX: Frame.io review player (precision scrub **behavior** — not a target UI to ship in the element crate).
- Implementation: **[mediabunny](https://github.com/Vanilagy/mediabunny)** — adapt patterns into Rust; do **not** add it as a dependency. How: `AGENTS.md` Mediabunny-first; traps: `.ai/skills/media.md`. Mediabunny ships sinks + examples (no scrubber UI in the library); our element helpers are an extra Leptos layer over `fpmedia-web`.

---

## Surfaces

| Surface | Purpose |
| ------- | ------- |
| **`fpmedia-web` crate** | Engine — demux + frame index + `VideoPlayer`; audio under `audio/`. Using only this + hand-rolled demo UI ≈ mediabunny’s example style. |
| **`ThumbSink`** (engine, feature `thumbs`) | **Thumb / filmstrip generation as its own product** — like mediabunny `CanvasSink`: open track → sized bitmaps via `get` / `at_frames`, no player required. `VideoPlayer` / `<HoverThumb>` are clients (lease + Exact bridge + workload gates). Design: `architecture.md` § Play / scrub residency. |
| **`fpmedia-web-components` crate** | Element analogs + headless track helpers (below) — hide engine/transport/`pipeline` from host UI authors. **No** player chrome. |
| **Demo app** | `demo-player` (Trunk) — **all** player UI: file picker, URL open, transport buttons, keyboard (JKL/Space/arrows), progress look, ticks, strip layout/colors, thumb chrome, status. Not production player UI. |

### Element-layer helpers (shipped)

Headless / paint-inside components in `fpmedia-web-components` (compose in demo; chrome stays in demo):

| Piece | Role |
| ----- | ---- |
| `<Video>` / `VideoRef` | Stage + element API (open/play/seek/rate/volume). |
| `<Scrubber>` | Gesture + track container + context. Owns pointer → playhead + `scrub_pointer_*` / transport / scrub phases. Host: children + CSS; may observe `scrubbing`. Does **not** ship progress fill, ticks, or strip layout. |
| `<TimelineMeta kind>` | One timeline meta strip (`Fetch` / `Keyframes` / `Covered` / `Exact`). Hides `pipeline` → continuous `f/n` runs → 1 px canvas paint (same grid as tip / zebra). Host: `class` / color + DOM placement (overlay vs stacked rows). Mount leases `PipelineInterest` (Fetch vs Ready) so unused planes are not built each `sync_ui`. |
| `<HoverThumb>` | Hover preview: canvas **inside**; props `width` / `height` / `class`; `children` = chrome (border, timecode, frame #); exposes `frame` / `visible`. Position (`transform`) computed from scrubber `update(x, track_w, frame)`. Auto-wire when under `<Scrubber>`; manual API escape hatch OK. |
| **Sequence** (agreed, not shipped) | Playlist of files/URLs as **one** review timeline. Elements: composition over **2×** `<Video>` (live + idle). Chrome (multi-open, `[i]` readout, go-to): demo/host. Design: `architecture.md` § Sequence composition; steps: `state.md`. |

`video.pipeline` may remain for a **debug** panel — not required for scrubber UI. Math helpers (`playhead_width_*`, `scrub_frame_from_x`, `strip::*`) stay for custom chrome. Phases design: `architecture.md` § Scrub phases.

---

## Invariants

- **Primary position** = presentation `FrameId` (0-based), not `time * fps`.
- **Exact vs Play (agreed):** “Frame-precise” means **seek / step / scrub settle / Pause** — Exact decode to the aimed frame; paused canvas is that frame. **Continuous Play prioritizes smoothness** (cadence, A/V sync, no micro-stutter) over painting every presentation frame. Do **not** invent Play-only “show every frame” / soft-pace pipelines that fight realtime. Shuttle (≥2×) may drop overdue frames. Prefer mediabunny media-player present+clock patterns.
- Exact frame N uses keyframe cascade (nearest sync → decode → target), including B-frame / decode-order windows — for **seek / settle / step / Pause**, not as a Play smoothness tax.
- Layout/state must allow **1–2 media sources** (sequence live+idle now; **compare** dual-visible later) — never hard-assume a single source forever.
- **Sequence playlist (agreed, not shipped):** user opens an ordered list of files and/or URLs; playback/scrub feel like **one** media. Scrubber axis = **duration** (wall time), not Σ frame counts — mixed fps keeps native rate per clip. Exactly **two** `<Video>` slots: **live** plays / holds aim; **idle** preloads the neighbor (open + warm; no full densify/thumb budget). Engine `VideoPlayer` stays single-clip. Auto-advance on clip EOF; **short seam hitch OK** (gapless A/V not v1). Cut markers on the bar — later. Design: `architecture.md` § Sequence composition.
- **Sequence position UI (agreed):** readout and go-to use **current clip only** for time/frame/TC — not one global SMPTE/FrameId across the playlist. Format: `[i] now / end` where `i` is **1-based** playlist index — Timecode e.g. `[1] 00:00:01:23 / 00:00:11:25`, Frames e.g. `[2] 10 / 100`. **Go-to:** `[i] <tc|frame>` jumps that clip; bare `tc`/`frame` (v1) = **current clip only**. Clip **name** in readout/go-to — later. Vocabulary: `glossary.md` § SequencePos.
- `<Video>` stage lays out like a sized `<video>`: fixed viewport, **contain**-fit picture, metadata via the element API.
- Continuous play keeps **A/V sync** on an audio-driven clock — also for clips without an audio track. The element API exposes volume / mute; it does not expose the clock. Clock design: `architecture.md` § Audio + media clock.
- **Audio codecs (agreed):** AAC (`mp4a`) and common QT/ISO **PCM** (`in24` / `sowt` / `twos` / …) play through the same volume/mute/rate path; PCM does not use WebCodecs `AudioDecoder`. Unsupported audio sample entries are ignored (video still opens).
- **Elements first, UI in demo only (agreed):** primary product = element + headless helpers above. Player chrome (buttons, progress look, ticks, strip composition, thumb labels/borders, keyboard) lives **only** in the host/demo. Do **not** grow production-styled player UI inside `fpmedia-web-components`. Flag-soup props (`show_progress`, `strip_layout`) on one mega-scrubber are out — compose instead.
- **Element styling namespace (agreed):** `fpmedia-web-components` ships **no stylesheet**. Public class hooks use the prefix **`fpvideo-`** (e.g. `fpvideo-stage` / `fpvideo-viewport` / `fpvideo-canvas`) so hosts can target structure without fragile tag selectors; override via `class` / `viewport_class` / `canvas_class` / `<HoverThumb class>`. Demo/host chrome classes (`wm-*`, etc.) are **not** part of the element contract. Hook table: `components/README.md` § Styling.
- **Hover thumbs:** Frame.io-style timeline preview. **Agreed UX:** Play **and** Pause, not while scrubbing; must not drive densify / buffer strips. Play paints from cache / Exact bridge only (no thumb generation). Pause progressive-fills key thumbs across the timeline; cache keeps an even grid via progressive rank (dense/Exact yield first at cap). Hover uses the same ranks and refines under the pointer. On `(hover: none)` (typical phone/tablet) — thumbs off entirely (`ThumbPolicy::Off`, no progressive / no `<HoverThumb>`); touch laptops with a mouse/trackpad usually keep `(hover: hover)`. Chrome around the paint surface = host (`<HoverThumb>` children / `class`).
- **Play first, scrub second:** smooth **1× play** must work for any supported clip (incl. 4K). Exactness is Pause/seek/step — not a reason to slow Play. Scrub/densify yields while playing. Do not retune Full-HD play defaults when tuning 4K.
- **Play vs scrub residency (agreed):** Play stays light — no background scrub caching while playing. Timeline meta kinds: **Fetch** (gray / fetched bytes), **Keyframes** (yellow / sync ticks; force ≥1 px), **Covered** (dim green / decoded coverage), **Exact** (bright green / step-ready); demo owns colors and whether strips overlay or stack. Strips stay crisp (no blur) on long clips. Green paint mirrors residency (small holes OK while densify fills; residual pin-holes after Pause densify on constrained decode are accepted strip cosmetics — Pause/seek Exact canvas stays correct — no soft-merge / min-px inflate). No separate buffer meter bars; live decode numbers live in the demo pipeline panel. Next/Prev near the playhead are instant when step-ready. Resize/fullscreen keeps the strips. Design: `architecture.md` § Play / scrub residency.
- **Playback rate:** review slider `0.25…1.75` (demo); API up to **8×**. Demo **JKL** (forward only): `1, 2, 4, 8` — **J** slower, **L** faster, **K** → **1×** + play/pause (**Space** toggles without resetting rate). Fixed-pitch audio through **4×**; **8×** silent. Reverse playback removed (open work: `state.md` Later if revisited).
- **Play at last frame (agreed):** Play while on the last frame restarts from frame 0 — **loop on or off** (same gesture). Do not arm the clock on the last sample (½ s audio blip then End). Matches mediabunny media-player at `endTimestamp`.

---

## Non-goals (for now)

- Full FFmpeg-in-browser convert/mux toolkit (mediabunny’s full scope).
- Multi-container write path; focus read + decode of MP4 (AVC; **HEVC** opt-in via cargo features — off by default).
- Production auth, storage, or collaboration UI (lives in host apps).
- Production player UI — demo controls in `app` are temporary.
- Production CORS / open media proxy — local progressive URL = static `/media/` files; Pages has no media proxy.
