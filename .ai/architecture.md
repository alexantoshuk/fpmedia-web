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
| Media element | `media_components` crate — Leptos `<Video>` / `VideoRef` (API-only) + `helpers` (scrubber CSS math) + `HoverThumbPreview` chrome. Modules: `video`, `helpers`, `hover_thumb` (+ planned `audio`) |
| Media engine | `webmedia` crate — demux + frame index + `VideoPlayer`; wasm WebCodecs in `decode/`; **`audio/`** for demux/stretch/output |
| Browser helpers (UI) | **`leptos-use`** for observers / DOM events / rAF — prefer over raw `web-sys` in `media_components/` / `app/` |
| Styling | Tailwind in `style/input.css` |

### Leptos UI rules

Long-lived chrome lives **outside** matched routes when routes are added. **Resources:** `resource.get()` in view closures is fine in CSR; show loading UI while `None`. Details: `skills/leptos.md`.

**Browser APIs in `media_components/` / `app/`:** prefer [`leptos-use`](https://leptos-use.rs/) (`use_resize_observer`, `use_event_listener`, `use_raf_fn`, …) over hand-rolled `web-sys` + `Closure`. Keep raw `web-sys` for media surfaces (`HtmlCanvasElement`, `Performance`, File) and anything the media crate owns (WebCodecs / `AudioContext`). The **media clock** (wall or audio) is owned by `webmedia` (`MediaClock` / `AudioOutput`) — hosts call `VideoPlayer::media_time()`, they do not open `AudioContext` themselves.

---

## Layout

```
webmedia/
├── media/                 ← crate name `webmedia`: demux + FrameIndex + Player + VideoPlayer
├── media_components/      ← crate name `media_components`: Leptos <Video> (+ later <Audio>)
├── timecode/              ← crate name `timecode`: SMPTE frames ↔ HH:MM:SS:FF (DF/NDF)
├── app/                   ← demo host UI + shell()
├── frontend/              ← wasm mount (hydrate)
├── server/                ← Axum static site
├── e2e/                   ← Playwright Chromium smoke (WebCodecs)
├── style/input.css
├── public/                ← sample MP4 fixtures served at /
└── .ai/                   ← product / architecture / state / skills
```

`media/src` module map (engine triage): `.ai/skills/media.md`.

### Layer rules

| Layer | Can import from |
| ----- | --------------- |
| `timecode/` | nothing else in the workspace (leaf math) |
| `media/` (`webmedia`) | `timecode`; nothing in `media_components` / `app` / Leptos |
| `media_components/` | `webmedia` (with `webcodecs`), Leptos, `leptos-use` |
| `app/` | `media_components` (csr), Leptos — not `webmedia` directly |
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
| Isolated `media_components` crate | HTML `<video>` / `<audio>` analogs; reusable without demo chrome |
| WebCodecs + canvas | Frame-accurate decode; stage mirrors sized `<video>` (contain + metadata) |
| `leptos-use` for browser helpers | Observers/events/rAF in `media_components/`; media crate stays free of Leptos |
| Adapt mediabunny ideas in Rust | Required reference; port patterns, never depend on the TS package (`AGENTS.md`) |
| Progressive open via `MediaSource` | Ranged reads (`BlobSource` / `UrlSource` / `MemorySource` + `CachedSource`); parse `moov` (+ `moof` for single-file fMP4), fetch samples by offset. Keep `re_mp4` for sample tables. Do **not** `array_buffer()` the whole file into WASM. |
| Dual decoder + scrub / play caches | Dual `VideoDecoder` so background fill does not steal playback. Stores, budgets, scrub pipeline, **`GopProfile`**: `.ai/skills/media.md`. |
| Optional hover thumbs (`thumbs`) | Compile: cargo feature `thumbs` **default on**. Runtime: `ThumbPolicy::Off \| On`. Lazy `ThumbPreview` / `ThumbSink`: ThumbCache + 3rd decoder + sparse key fill in `decode/`; player Exact bridge. Pure size/gap helpers in `types`. UI: `HoverThumbPreview`. `mediabunny:` separate `CanvasSink` / `getCanvas` / `canvasesAtTimestamps`. |
| Audio in engine, not `VideoRef` | Demux + `AudioDecoder` + `AudioContext` + WSOLA live in `webmedia::audio`. `VideoPlayer` orchestrates A+V; `VideoRef` only volume/mute **API**. Keyboard/chrome stay in demo `app`. Future `<Audio>` shares `audio/` (no canvas). mediabunny: `AudioBufferSink` + `examples/media-player`. |
| Component API like HTML media | Element crates expose open/play/seek/rate/volume — not host keybindings. Scrubber strip math: `media_components::helpers`. Demo owns JKL/Space/arrows. |

---

## Progressive open (MediaSource)

**Shipped (wasm demo):** `MediaSource` + `BlobSource` / `UrlSource` / `MemorySource` + `CachedSource` (512 MiB byte-budget LRU, 8 MiB quantum + hit-path margin prefetch / network grow; mediabunny `UrlSource` default is 64 MiB — we keep more for high-RTT `/proxy`); scrubber gray strip mirrors resident windows via `FetchMap`. Top-level `ftyp`+`moov` (+ full `moof` bodies for single-file fMP4) → `re_mp4`; `Mp4VideoTrack` async `encoded_sample`; UI `open_blob(File)` without whole-file `array_buffer()`; `open_url` rewrites remote `http(s)` to same-origin **`/proxy?url=`** (demo Axum forwards `Range`; bypasses GitHub Release / S3 CORS), then HTTP Range when the upstream supports `206` (else full-download fallback). Async AVC sync verify after moov (`verify_avc_sync: true`). `open_mp4_bytes` kept for fixtures/tests.

**Demo `/proxy` policy (fixed):** localhost-only for local tests / e2e. TCP peer must be loopback (`127.0.0.1` / `::1`, incl. IPv4-mapped); otherwise **403**. Keep `site-addr` on loopback. Not a production CORS proxy — no DNS-resolve / allowlist hardening. Host apps that need remote media supply their own same-origin fetch path.

**Still open** (see `.ai/state.md`): native `FileSource`; optional lazy/background sync-verify for huge files; coalesce adjacent sample ranges; UrlSource parallel workers / sequential no-Range stream (mediabunny orchestrator).

**Out of scope:** stream-only sources without random access; live / growing fMP4; DASH/HLS segment playlists; MSE remux; production open media proxy.

**Success bar:** 1080p progressive open to first paint without allocating the full media size in WASM.

### Test media

| Kind | Where |
| ---- | ----- |
| Tiny unit/smoke MP4s | `media/tests/fixtures/` (+ copies in `public/` for e2e smoke) |
| FullHD A/V sync plates | **[testvideo](https://github.com/alexantoshuk/testvideo)** Releases — catalog `e2e/helpers/testvideo.ts` |

Do **not** document or wire probes to Downloads / third-party sample hosts. In skills/gotchas prefer **meta** (fps, GOP, resolution, bf) over ad-hoc filenames; when naming an asset, use the testvideo stem (`avc_1080p_24_gop48_bf0`, …).

---

## Code rules

- Gate server-only code: `#[cfg(feature = "ssr")]`.
- Gate client UI: `#[cfg(feature = "csr")]`.
- Errors: `thiserror`; no `.unwrap()`/`.expect()` in production paths.
- WebCodecs needs `web_sys_unstable_apis` (`.cargo/config.toml`).
- **WebCodecs / media resources:** RAII owners + borrow helpers (`OwnedVideoFrame` / `AsVideoFrame`); **move into caches**, **borrow to draw**. Gotchas: `skills/media.md`.
- **Mediabunny reference:** before demux / decode / seek / playback work, check [Vanilagy/mediabunny](https://github.com/Vanilagy/mediabunny); reimplement in Rust (`AGENTS.md`).
