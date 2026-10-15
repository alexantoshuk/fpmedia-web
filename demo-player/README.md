# fpmedia-web demo player

Trunk CSR demo: Browse (Blob) + URL open. Player chrome lives here; elements come from
[`fpmedia-web-components`](../components).

## Run

```sh
rustup target add wasm32-unknown-unknown
cargo install trunk   # once
cd demo-player
trunk serve           # auto-downloads Tailwind CLI (`[tools] tailwindcss` in Trunk.toml)
```

Open http://127.0.0.1:3000 — file picker or URL.

### Progressive URL (gray Fetch strip)

Same-origin files under `public/media/` (gitignored). Fetch from
[testvideo](https://github.com/alexantoshuk/testvideo) v0.3.0:

```sh
# from repo root
npm run media:fetch              # default e2e set
npm run media:fetch -- gop12     # one alias / stem
```

Then open `/media/avc_1080p_25_gop12_bf0.mp4` (or paste the GitHub download URL — the
demo rewrites known `…/download/v0.3.0/…` to `/media/…`).

No CORS proxy. Pages has no large masters — Browse (Blob) or your own CORS-ok URL.

## Build (Pages / static)

```sh
cd demo-player
trunk build --release --public-url /fpmedia-web/
# serve dist/ with any static host
```
