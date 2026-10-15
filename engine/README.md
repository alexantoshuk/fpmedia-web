# fpmedia-web

[![docs.rs](https://img.shields.io/badge/docs-docs.rs-blue)](https://docs.rs/fpmedia-web)
[![crates.io](https://img.shields.io/badge/crates.io-fpmedia--web-orange)](https://crates.io/crates/fpmedia-web)

Frame-accurate media **engine** for the browser: MP4 demux + presentation frame index +
`VideoPlayer` (WebCodecs → canvas). No Leptos UI.

## Features

| Feature | Default | Role |
| ------- | ------- | ---- |
| `thumbs` | on | Hover / filmstrip thumb decode path |
| `hevc` | off | H.265 open + bitstream helpers |
| `webcodecs` | off | WASM WebCodecs decode (required for `VideoPlayer` in browser) |

Hosts that only demux on native can omit `webcodecs`. Browser players enable it
(see `fpmedia-web-leptos`).

## Dependency note

`re_mp4` is pinned to a **git** rev until QT PCM / SoundDescription fixes land on
crates.io (rerun-io/re_mp4 #35 + #38). Do not publish this crate until that pin
can switch to a crates.io version.

## Docs

- Product / UX: repo `.ai/product.md`
- Layout / policy: `.ai/architecture.md`
- Engine traps: `.ai/skills/media.md`
- Element layer + CORS: [`../leptos/README.md`](../leptos/README.md)
- Minimal Blob CSR host (no Axum): [`../examples/blob_open`](../examples/blob_open)

License: MIT OR Apache-2.0 (see repo `LICENSE`).
