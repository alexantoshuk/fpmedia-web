# Contributing to webmedia

## Commits

Semantic format:

```
<type>(<scope>): <short description>
```

| Type | When to use |
| ---------- | ---------------------------- |
| `feat` | new feature/behaviour |
| `fix` | bug fix |
| `refactor` | code change, not feat/fix |
| `style` | formatting, CSS, no logic |
| `test` | tests |
| `chore` | tooling, deps, config, build |
| `docs` | docs only |

- Subject ≤ 72 chars, imperative (`add`, `fix`, `remove` — not `added`).
- Scope optional: `feat(player)`, `fix(webcodecs)`.
- Closes a `.ai/state.md` task → blank line then `closes: <task name>`.
- Include `.ai/` updates when architecture, player UI, or a skill-worthy fix changed. No long duplicate prose across product, architecture, and skills.
- Related `.ai/` updates may ship with the code they describe.

### Pre-commit / pre-push gates

Do **not** commit or open/update a PR until these pass locally. `cargo check` alone is not enough — Clippy (`-D warnings`) is required.

```sh
cargo fmt --all -- --check
cargo clippy -p webmedia --all-targets --all-features -- -D warnings
cargo clippy --target wasm32-unknown-unknown -p frontend -- -D warnings
cargo clippy --target wasm32-unknown-unknown -p video -- -D warnings
cargo check -p server
cargo check --target wasm32-unknown-unknown -p frontend
cargo check --target wasm32-unknown-unknown -p video
cargo test -p webmedia
cargo leptos build
```

- Always run `cargo fmt` (rustfmt) before committing Rust changes; include any fmt-only touch-ups in the same commit.
- Never commit code failing the gates above.
- For UI/player behaviour changes that e2e covers: also run `npm run e2e` with the app up on `:3000` when practical.

### Commit / PR size

Soft guidance. Split when a change exceeds “too much” unless it is inherently
atomic (rename/format, lockfile, generated assets).

| Metric | Optimal | Acceptable | Too much |
| ------ | ------- | ---------- | -------- |
| Lines changed | 50–200 | 200–400 | > 400 |
| Files changed | 1–10 | 10–20 | > 20 |
| Commits per PR | 1–5 | 5–10 | > 10 |

- One commit = one logical theme.
- Prefer several small commits over one large mixed commit.
- Line counts exclude lockfiles and generated files when judging size.
