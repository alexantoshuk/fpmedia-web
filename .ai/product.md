# Product

> Stable domain and UX context. Read at start of every session.
> Change only after a product decision proves itself across sessions.

---

## What

**webmedia** — frame-accurate custom web media player for precise frame-by-frame review in the browser.

Alpha goal: open an MP4, seek/step by presentation frame, play/pause via WebCodecs → canvas. Not a generic `<video>` wrapper.

**References**: Frame.io review player (precision scrub); [mediabunny](https://github.com/Vanilagy/mediabunny) (WebCodecs demux/decode patterns to adapt — not a dependency).

---

## Surfaces

| Surface | Purpose |
| ------- | ------- |
| **Demo app** | Leptos CSR SPA — load file / sample fixture, canvas preview, transport controls |
| **Library API** | `webmedia` crate — demux + frame index + `Player` / `WebPlayer` (consumable by other apps) |

---

## Invariants

- **Primary position** = presentation `FrameId` (0-based), not `time * fps`.
- Exact frame N uses keyframe cascade (nearest sync → decode → target), including B-frame / decode-order windows.
- Player layout/state must allow **1–2 media sources** later (compare) — never hard-assume a single source forever.
- No audio in the first alpha slice; continuous play + audio come next.

---

## Non-goals (for now)

- Full FFmpeg-in-browser convert/mux toolkit (mediabunny’s full scope).
- Multi-container write path; focus read + decode of MP4/H.264 first.
- Production auth, storage, or collaboration UI (lives in host apps).
