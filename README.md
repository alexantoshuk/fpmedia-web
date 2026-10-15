# fpmedia-web

[![pages](https://github.com/alexantoshuk/fpmedia-web/actions/workflows/pages.yml/badge.svg)](https://github.com/alexantoshuk/fpmedia-web/actions/workflows/pages.yml)
[![docs fpmedia-web](https://img.shields.io/badge/docs-fpmedia--web-blue)](https://docs.rs/fpmedia-web)
[![docs fpmedia-web-components](https://img.shields.io/badge/docs-fpmedia--web--components-blue)](https://docs.rs/fpmedia-web-components)
[![docs fpmedia-timecode](https://img.shields.io/badge/docs-fpmedia--timecode-blue)](https://docs.rs/fpmedia-timecode)

Frame-accurate custom web media player (Rust + Leptos + WebCodecs).

Crate guides (until crates.io / docs.rs publish): [`engine/`](engine/README.md) · [`components/`](components/README.md) · [`timecode/`](timecode/README.md).

**Live demo:** [alexantoshuk.github.io/fpmedia-web](https://alexantoshuk.github.io/fpmedia-web/) (Browse / Blob only — no public `/proxy`).

| Dir | Crate | Role |
| --- | ----- | ---- |
| [`engine/`](engine/) | `fpmedia-web` | demux + `VideoPlayer` |
| [`components/`](components/) | `fpmedia-web-components` | `<Video>` + headless scrub helpers |
| [`timecode/`](timecode/) | `fpmedia-timecode` | SMPTE frames ↔ HH:MM:SS:FF |
| [`examples/blob_open/`](examples/blob_open/) | (demo) | minimal Blob CSR host — **no Axum** |

Element open contract (CORS / no auto `/proxy`): [`components/README.md`](components/README.md).

**Pages build:** [`.github/workflows/pages.yml`](.github/workflows/pages.yml) (`--lib-features static_host`). Local export:

```sh
# optional: FPMEDIA_SITE_BASE=/fpmedia-web for project-pages asset prefix
cargo leptos build --release --lib-features static_host
cargo run --release -p server --bin export_site
# serve target/site/ with any static host
```

License: [MIT OR Apache-2.0](LICENSE). Repo: [alexantoshuk/fpmedia-web](https://github.com/alexantoshuk/fpmedia-web).

Dev commands: [`AGENTS.md`](AGENTS.md) § Dev commands. Commit / PR gates: [`CONTRIBUTING.md`](CONTRIBUTING.md).
