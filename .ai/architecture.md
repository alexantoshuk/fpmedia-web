# Architecture

> Stable stack and code-structure context. Read at start of every session.
> Change only after a technical decision proves itself across sessions.

---

## Stack

| Piece | Choice |
| ----- | ------ |
| Language | Rust, full-stack |
| Framework | Leptos CSR — `mount_to_body` SPA; static `index.html` shell |
| Server shell | Axum (`server/`) — static files, optional fixture serving |
| Client shell | `frontend/` — `mount_to_body(App)` (export name `hydrate` for cargo-leptos) |
| App UI | `app` crate — demo host (controls) + thin SSR shell |
| Media element | `video` crate — Leptos `<Video>` + `VideoRef` over `webmedia` |
| Media engine | `webmedia` crate — demux + frame index + player; wasm WebCodecs |
| Browser helpers (UI) | **`leptos-use`** for observers / DOM events / rAF — prefer over raw `web-sys` in `video/` / `app/` |
| Styling | Tailwind in `style/input.css` |

### Leptos UI rules

Long-lived chrome lives **outside** matched routes when routes are added. **Resources:** `resource.get()` in view closures is fine in CSR; show loading UI while `None`. Details: `skills/leptos.md`.

**Browser APIs in `video/` / `app/`:** prefer [`leptos-use`](https://leptos-use.rs/) (`use_resize_observer`, `use_event_listener`, `use_raf_fn`, …) over hand-rolled `web-sys` + `Closure`. Keep raw `web-sys` for media-specific surfaces (`AudioContext`, `HtmlCanvasElement`, File) and anything the media crate owns (WebCodecs).

---

## Layout

```
webmedia/
├── media/                 ← crate name `webmedia`: demux + FrameIndex + Player + WebPlayer
├── video/                 ← crate name `video`: Leptos <Video> + VideoRef
├── app/                   ← demo host UI + shell()
├── frontend/              ← wasm mount (hydrate)
├── server/                ← Axum static site
├── e2e/                   ← Playwright Chromium smoke (WebCodecs)
├── style/input.css
├── public/                ← sample MP4 fixtures served at /
└── .ai/                   ← product / architecture / state / skills
```

### Layer rules

| Layer | Can import from |
| ----- | --------------- |
| `media/` (`webmedia`) | nothing in `video` / `app` / Leptos (leaf engine) |
| `video/` | `webmedia` (with `webcodecs`), Leptos, `leptos-use` |
| `app/` | `video` (csr), Leptos — not `webmedia` directly |
| `frontend/` | `app` |
| `server/` | `app` (ssr shell only) |

**Feature flags** on `app`: `ssr` (server binary) and `csr` (WASM client). Not enabled together on one build.

---

## Architectural decisions

| Decision | Why |
| -------- | --- |
| Full-stack Rust (Leptos) | One language for engine + UI |
| CSR SPA | Simple mental model; static shell + WASM mount |
| Isolated `webmedia` crate | Engine reusable without UI; no Leptos in demux/decode |
| Isolated `video` crate | HTML `<video>` analog; reusable without demo chrome |
| WebCodecs + canvas | Frame-accurate decode; stage mirrors sized `<video>` (contain + metadata) |
| `leptos-use` for browser helpers | Observers/events/rAF in `video/`; media crate stays free of Leptos |
| Adapt mediabunny ideas in Rust | [mediabunny](https://github.com/Vanilagy/mediabunny) is the required reference; port patterns, never depend on the TS package |
| Progressive open via `MediaSource` (planned) | Match `<video>` / mediabunny `BlobSource`: ranged reads, parse `moov` only, fetch `mdat` samples by file offset. Keep `re_mp4` for sample tables. Do **not** `array_buffer()` the whole file into WASM. |
| Dual decoder + scrub / play caches | Dual `VideoDecoder` so background fill does not steal playback. Stores, budgets, scrub pipeline, **`GopProfile` policy**: `.ai/skills/media.md` (Frame and decode caches). |

---

## Progressive open (MediaSource)

**Problem:** open loads the entire MP4/MOV into a `Vec<u8>` (`file.array_buffer` → `open_mp4_bytes`), so ~700 MB clips stall before first frame.

**Approach** (mediabunny-aligned; keep `re_mp4`):

1. ~~`MediaSource`~~ — `MemorySource` + wasm `BlobSource`; ~8 MiB `CachedSource`. Native `FileSource` later.
2. ~~Top-level box scan~~ — `ftyp`+`moov` stub → `re_mp4` (file-absolute sample offsets).
3. ~~`Mp4VideoTrack` holds source~~ — async `encoded_sample` via `source.read`.
4. ~~UI `open_blob(File)`~~ — demo no longer `array_buffer()`s the whole file.
5. Defer full AVC sync-flag verify off the critical path (lazy / first GOP).
6. Keep `open_mp4_bytes` for fixtures/tests (offset-consistency covered).

**Out of scope for this track:** stream-only sources without random access; fMP4/DASH; MSE remux.

**Success:** BBB-class file opens to first paint without allocating the full media size in WASM.

Details / ordering: `.ai/state.md` (In progress).

---

## Code rules

- Gate server-only code: `#[cfg(feature = "ssr")]`.
- Gate client UI: `#[cfg(feature = "csr")]`.
- Errors: `thiserror`; no `.unwrap()`/`.expect()` in production paths.
- WebCodecs needs `web_sys_unstable_apis` (`.cargo/config.toml`).
- **WebCodecs / media resources:** wrap types that need explicit release or unsafe clone in RAII owners + borrow helpers (pattern: `OwnedVideoFrame` / `AsVideoFrame`). Prefer **move into caches** and **borrow to draw**; never silent `Option` clone for residency. Do **not** wrap ordinary DOM / `Blob` / `window` / `console` handles.
- **Mediabunny reference:** before implementing or fixing demux / decode / seek / playback pacing, check [Vanilagy/mediabunny](https://github.com/Vanilagy/mediabunny) (docs + source) for the analogous approach; reimplement in Rust.
