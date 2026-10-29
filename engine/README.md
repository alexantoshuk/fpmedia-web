# fpmedia-web

[![version](https://img.shields.io/badge/version-0.1.0--alpha.1-lightgrey)](../Cargo.toml)
[![publish](https://img.shields.io/badge/crates.io-pending-lightgrey)](../.ai/state.md)

Frame-precise media **engine** for the browser: MP4 demux + presentation frame index +
`VideoPlayer` (WebCodecs → canvas). No Leptos UI.

## Features

| Feature | Default | Role |
| ------- | ------- | ---- |
| `thumbs` | on | Hover / filmstrip thumb decode path |
| `hevc` | off | H.265 open + bitstream helpers |
| `webcodecs` | off | WASM WebCodecs decode (required for `VideoPlayer` in browser) |

Hosts that only demux on native can omit `webcodecs`. Browser players enable it
(see `fpmedia-web-components`).

## Dependency note

`re_mp4` is pinned to a **git** rev until QT PCM / SoundDescription fixes land on
crates.io (rerun-io/re_mp4 #35 + #38). Do not publish this crate until that pin
can switch to a crates.io version.

## Docs

- Product / UX: repo `.ai/product.md`
- Layout / policy: `.ai/architecture.md`
- Engine traps: `.ai/skills/media.md`
- Element layer + CORS: [`../components/README.md`](../components/README.md)
- Demo host (Trunk): [`../demo-player`](../demo-player)

License: MIT OR Apache-2.0 (see repo `LICENSE`).
