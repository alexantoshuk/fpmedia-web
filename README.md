# fpmedia-web

[![pages](https://github.com/alexantoshuk/fpmedia-web/actions/workflows/pages.yml/badge.svg)](https://github.com/alexantoshuk/fpmedia-web/actions/workflows/pages.yml)
[![docs fpmedia-web](https://img.shields.io/badge/docs-fpmedia--web-blue)](https://docs.rs/fpmedia-web)
[![docs fpmedia-web-components](https://img.shields.io/badge/docs-fpmedia--web--components-blue)](https://docs.rs/fpmedia-web-components)
[![docs fpmedia-timecode](https://img.shields.io/badge/docs-fpmedia--timecode-blue)](https://docs.rs/fpmedia-timecode)

Frame-precise custom web media player (Rust + Leptos + WebCodecs).

Crate guides (until crates.io / docs.rs publish): [`engine/`](engine/README.md) · [`components/`](components/README.md) · [`timecode/`](timecode/README.md).

**Live demo:** [alexantoshuk.github.io/fpmedia-web](https://alexantoshuk.github.io/fpmedia-web/) (Browse / Blob; URL needs CORS-ok origin — no public media proxy).

| Dir | Crate | Role |
| --- | ----- | ---- |
| [`engine/`](engine/) | `fpmedia-web` | demux + `VideoPlayer` |
| [`components/`](components/) | `fpmedia-web-components` | `<Video>` + headless scrub helpers |
| [`timecode/`](timecode/) | `fpmedia-timecode` | SMPTE frames ↔ HH:MM:SS:FF |
| [`demo-player/`](demo-player/) | (demo) | Trunk CSR host — Browse + URL |

Element open contract (CORS / no auto proxy): [`components/README.md`](components/README.md).

**Dev demo:**

```sh
cd demo-player && trunk serve   # http://127.0.0.1:3000
npm run media:fetch             # optional: testvideo → public/media/ for progressive URL
```

**Pages build:** [`.github/workflows/pages.yml`](.github/workflows/pages.yml):

```sh
cd demo-player
trunk build --release --public-url /fpmedia-web/
# serve dist/ with any static host
```

License: [MIT OR Apache-2.0](LICENSE). Repo: [alexantoshuk/fpmedia-web](https://github.com/alexantoshuk/fpmedia-web).

Dev commands: [`AGENTS.md`](AGENTS.md) § Dev commands. Commit / PR gates: [`CONTRIBUTING.md`](CONTRIBUTING.md).
