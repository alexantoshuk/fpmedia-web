# fpmedia-web-leptos

Leptos element analogs (`<Video>`, `<Scrubber>`, `<TimelineMeta>`, `<HoverThumb>`)
over the [`fpmedia-web`](../engine) engine. **No** player chrome — compose in the host.

## Open / CORS contract

| API | Host responsibility |
| --- | ------------------- |
| `VideoRef::open_blob` | Pass a `Blob` (file picker, prior fetch). |
| `VideoRef::open_url` | Pass a URL the **browser** can fetch with Range (or full GET). Cross-origin hosts must send CORS headers that allow your origin; otherwise open fails. |

This crate does **not** rewrite URLs to a `/proxy`. Same-origin media proxies (demo Axum `/proxy`, your CDN edge, etc.) are **host-owned**: rewrite before calling `open_url`, or only open same-origin / CORS-ok assets.

Prefer Blob open for local files. For progressive URL open, the server should support HTTP `Range` (`206` + `Content-Range`); without Range the element falls back to a full download into a Blob.

## Features

- `thumbs` (default) — hover thumb decode path
- `hevc` — opt-in H.265 (see engine crate)

## Minimal host example

[`examples/blob_open`](../examples/blob_open) — file picker → `open_blob`, Play/Pause, no Axum / no `/proxy`.

## Docs

Product surfaces: repo `.ai/product.md`. Host traps: `.ai/skills/leptos.md`.
