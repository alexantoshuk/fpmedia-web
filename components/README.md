# fpmedia-web-components

[![docs.rs](https://img.shields.io/badge/docs-docs.rs-blue)](https://docs.rs/fpmedia-web-components)
[![crates.io](https://img.shields.io/badge/crates.io-fpmedia--web--components-orange)](https://crates.io/crates/fpmedia-web-components)

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

## Demo host

[`demo-player/`](../demo-player) — Trunk CSR: file picker + URL open (`trunk serve`).

## Docs

Product surfaces: repo `.ai/product.md`. Host traps: `.ai/skills/leptos.md`.
