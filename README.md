# fpmedia-web

Frame-accurate custom web media player (Rust + Leptos + WebCodecs).

**Live demo:** [alexantoshuk.github.io/fpmedia-web](https://alexantoshuk.github.io/fpmedia-web/) (Browse / Blob only — no public `/proxy`).

| Dir | Crate | Role |
| --- | ----- | ---- |
| [`engine/`](engine/) | `fpmedia-web` | demux + `VideoPlayer` |
| [`leptos/`](leptos/) | `fpmedia-web-leptos` | `<Video>` + headless scrub helpers |
| [`timecode/`](timecode/) | `fpmedia-timecode` | SMPTE frames ↔ HH:MM:SS:FF |
| [`examples/blob_open/`](examples/blob_open/) | (demo) | minimal Blob CSR host — **no Axum** |

Element open contract (CORS / no auto `/proxy`): [`leptos/README.md`](leptos/README.md).

**Pages build:** [`.github/workflows/pages.yml`](.github/workflows/pages.yml) (`--lib-features static_host`). Local export:

```sh
# optional: FPMEDIA_SITE_BASE=/fpmedia-web for project-pages asset prefix
cargo leptos build --release --lib-features static_host
cargo run --release -p server --bin export_site
# serve target/site/ with any static host
```

License: [MIT OR Apache-2.0](LICENSE). Repo: [alexantoshuk/fpmedia-web](https://github.com/alexantoshuk/fpmedia-web).

Dev commands: [`AGENTS.md`](AGENTS.md) § Dev commands. Commit / PR gates: [`CONTRIBUTING.md`](CONTRIBUTING.md).
