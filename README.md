# webmedia

Frame-accurate custom web media player (Rust + Leptos + WebCodecs).

## Dev

```sh
cargo leptos watch   # http://127.0.0.1:3000
cargo test -p webmedia

npm install          # once
npm run e2e          # Playwright smoke (app must be up; Windows uses system Chrome)
```

See `AGENTS.md` and `.ai/` for the agent workflow.
