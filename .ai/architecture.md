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
| App UI | `app` crate — shared CSR client + thin SSR shell |
| Media engine | `webmedia` crate — demux + frame index + player; wasm WebCodecs |
| Styling | Tailwind in `style/input.css` |

### Leptos UI rules

Long-lived chrome lives **outside** matched routes when routes are added. **Resources:** `resource.get()` in view closures is fine in CSR; show loading UI while `None`. Details: `skills/leptos.md`.

---

## Layout

```
webmedia/
├── media/                 ← crate name `webmedia`: demux + FrameIndex + Player + WebPlayer
├── app/                   ← Leptos App + shell() + demo player UI
├── frontend/              ← wasm mount (hydrate)
├── server/                ← Axum static site
├── style/input.css
├── public/                ← sample MP4 fixtures served at /
└── .ai/                   ← product / architecture / state / skills
```

### Layer rules

| Layer | Can import from |
| ----- | --------------- |
| `media/` (`webmedia`) | nothing in `app` / Leptos (leaf engine) |
| `app/` | `webmedia` (with `webcodecs` on wasm), Leptos |
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
| WebCodecs + canvas | Frame-accurate decode; `<video>` may remain fallback later |
| Adapt mediabunny ideas in Rust | [mediabunny](https://github.com/Vanilagy/mediabunny) is the required reference for media features and decode/playback fixes; port patterns, never depend on the TS package |

---

## Code rules

- Gate server-only code: `#[cfg(feature = "ssr")]`.
- Gate client UI: `#[cfg(feature = "csr")]`.
- Errors: `thiserror`; no `.unwrap()`/`.expect()` in production paths.
- WebCodecs needs `web_sys_unstable_apis` (`.cargo/config.toml`).
- **Mediabunny reference:** before implementing or fixing demux / decode / seek / playback pacing, check [Vanilagy/mediabunny](https://github.com/Vanilagy/mediabunny) (docs + source) for the analogous approach; reimplement in Rust.
