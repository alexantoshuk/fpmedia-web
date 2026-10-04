# Product

> Stable domain and UX context. Read at start of every session.
> Change only after a product decision proves itself across sessions.
> Implementation traps → `.ai/skills/`; stack/design → `.ai/architecture.md`.

---

## What

**webmedia** — frame-accurate custom web media player for precise frame-by-frame review in the browser.

Alpha goal: open an MP4, seek/step/scrub by presentation frame, play/pause via WebCodecs → canvas. Not a generic `<video>` wrapper — a Rust/Leptos analog with frame-primary control.

**References**:
- UX: Frame.io review player (precision scrub).
- Implementation: **[mediabunny](https://github.com/Vanilagy/mediabunny)** — adapt patterns into Rust; do **not** add it as a dependency. How: `AGENTS.md` Mediabunny-first; traps: `.ai/skills/media.md`.

---

## Surfaces

| Surface | Purpose |
| ------- | ------- |
| **`media_components` crate** | Leptos `<Video>` + `VideoRef` — media element API only (HTML `<video>` analog). Modules: `video`, `helpers` (+ planned `audio`) |
| **Demo app** | `app` / `frontend` — file picker, transport buttons, **keyboard (JKL/Space/arrows)**, status (not production chrome) |
| **Library API** | `webmedia` crate — demux + frame index + `Player` / `VideoPlayer`; audio under `audio/` |

---

## Invariants

- **Primary position** = presentation `FrameId` (0-based), not `time * fps`.
- **Exact vs Play (agreed):** “Frame-accurate” means **seek / step / scrub settle / Pause** — Exact decode to the aimed frame; paused canvas is that frame. **Continuous Play prioritizes smoothness** (cadence, A/V sync, no micro-stutter) over painting every presentation frame. Do **not** invent Play-only “show every frame” / soft-pace pipelines that fight realtime. Shuttle (≥2×) may drop overdue frames. Prefer mediabunny media-player present+clock patterns.
- Exact frame N uses keyframe cascade (nearest sync → decode → target), including B-frame / decode-order windows — for **seek / settle / step / Pause**, not as a Play smoothness tax.
- Layout/state must allow **1–2 media sources** later (compare) — never hard-assume a single source forever.
- `<Video>` stage lays out like a sized `<video>`: fixed viewport, **contain** canvas, metadata via `VideoRef` / `VideoPlayer`.
- Continuous play timeline is driven by **`AudioContext`** when possible (AAC output, or silent playback clock with no audio track). Wall `performance` only for shuttle `>4×` / fallback. Audio decode lives in the **engine**, not `VideoRef` (volume/mute/transport API only). Future pure `<Audio>` reuses the same engine modules.
- **Component vs demo:** element crates expose **API only**. Scrubber strip math in `media_components::helpers`; keyboard / transport chrome in `app`.
- **Hover thumbs:** Frame.io-style timeline preview. **Agreed UX:** Play **and** Pause, not while scrubbing; must not drive densify / buffer strips.
- **Play first, scrub second:** smooth **1× play** must work for any supported clip (incl. 4K). Exactness is Pause/seek/step — not a reason to slow Play. Scrub/densify yields while playing. Do not retune Full-HD play defaults when tuning 4K.
- **Play vs scrub residency (agreed):** Play stays light (tiny live-frame pool, densify off). Scrub path = decode → bitmap → canvas. Demo scrubber: **gray** = fetched bytes, **dim green** = coverage, **bright green** = step-ready blit window — painted as 1 px-tall canvases + integer `fillRect` (not many-stop CSS gradients). No separate buffer meter bars; live decode numbers in `pipeline` / hidden `pipeline-json`. Seek/settle paint decode frames; Next/Prev prefer warm rings/blit. Resize/fullscreen keeps scrub trail.
- **Playback rate:** review slider `0.25…1.75` (demo); API up to **8×**. Demo **JKL** (forward only): `1, 2, 4, 8` — **J** slower, **L** faster, **K** → **1×** + play/pause (**Space** toggles without resetting rate). Fixed-pitch audio through **4×**; **8×** silent. Shuttle present policy from **2×**. Reverse playback removed (open work: `state.md` Later if revisited).

---

## Non-goals (for now)

- Full FFmpeg-in-browser convert/mux toolkit (mediabunny’s full scope).
- Multi-container write path; focus read + decode of MP4 (AVC; **HEVC** opt-in via cargo features — off by default).
- Production auth, storage, or collaboration UI (lives in host apps).
- Production player chrome — demo controls in `app` are temporary.
- Production CORS / open media proxy — demo `/proxy` is localhost-only (local tests / e2e).
