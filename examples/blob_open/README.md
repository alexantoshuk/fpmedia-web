# Blob open example (no Axum)

Minimal Leptos CSR host: file picker → [`VideoRef::open_blob`](../../leptos).
No URL open, no `/proxy`, no demo chrome.

## Run

Needs [Trunk](https://trunkrs.dev/) and a WASM target:

```sh
rustup target add wasm32-unknown-unknown
cargo install trunk   # once
cd examples/blob_open
trunk serve
```

Open the printed URL, pick a local MP4, then Play / Pause.

## Check (no browser)

From the repo root:

```sh
cargo check --target wasm32-unknown-unknown -p fpmedia-web-example-blob-open
```

CORS / URL open contract for real hosts: [`../../leptos/README.md`](../../leptos/README.md).
