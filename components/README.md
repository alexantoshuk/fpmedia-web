# fpmedia-web-components

[![version](https://img.shields.io/badge/version-0.1.0--alpha.1-lightgrey)](../Cargo.toml)
[![publish](https://img.shields.io/badge/crates.io-pending-lightgrey)](../.ai/state.md)

Leptos element analogs (`<Video>`, `<Scrubber>`, `<TimelineMeta>`, `<HoverThumb>`)
over the [`fpmedia-web`](../engine) engine. **No** player chrome — compose in the host.

## Open / CORS contract

| API | Host responsibility |
| --- | ------------------- |
| `VideoRef::open_blob` | Pass a `Blob` (file picker, prior fetch). |
| `VideoRef::open_url` | Pass a URL the **browser** can fetch with Range (or full GET). Cross-origin hosts must send CORS headers that allow your origin; otherwise open fails. |

This crate does **not** rewrite URLs. Same-origin media facades (demo Trunk `/media/`, your CDN edge, etc.) are **host-owned**: rewrite before calling `open_url`, or only open same-origin / CORS-ok assets.

Prefer Blob open for local files. For progressive URL open, the server should support HTTP `Range` (`206` + `Content-Range`); without Range the element falls back to a full download into a Blob.

## Features

- `thumbs` (default) — hover thumb decode path
- `hevc` — opt-in H.265 (see engine crate)

## Styling

Principle (namespace `fpvideo-*`, no stylesheet, demo chrome ≠ contract): repo `.ai/product.md` § Invariants → Element styling namespace.

This crate ships **no stylesheet**. Stable class hooks (override via props):

| Hook | Default class | Prop |
| ---- | ------------- | ---- |
| Stage root | `fpvideo-stage` | `Video` `class` |
| Viewport | `fpvideo-viewport` | `viewport_class` |
| Canvas | `fpvideo-canvas` | `canvas_class` |
| Hover thumb | (none / host) | `HoverThumb` `class` — demo uses `fpvideo-thumb-preview` |

Prefix `fpvideo-` is the public styling namespace; avoid colliding with host CSS.
Demo chrome (`wm-*` controls, scrubber layout) stays host-only.

## Demo host

[`demo-player/`](../demo-player) — Trunk CSR: file picker + URL open (`trunk serve`).

## Docs

Product surfaces: repo `.ai/product.md`. Host traps: `.ai/skills/leptos.md`.
