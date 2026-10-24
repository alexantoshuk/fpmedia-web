# Contributing to fpmedia-web

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
- **`fix` body (UX):** we are not filing / closing GitHub Issues for now, so the commit body must briefly say what broke for the user (choppy audio after scrub at 2×, frozen scrub canvas, …). Implementation detail may follow; do not ship a fix that only names symbols.
- **State with the feature (hard):** if the commit ships work tracked in `.ai/state.md`, delete that bullet from state **before** committing and include the updated `state.md` in the **same** commit as the code. Do not leave a finished item in In progress / Next up / Later, and do not commit the feature without the state drop.
- Include `.ai/` updates when architecture, player UI, or a skill-worthy fix changed. No long duplicate prose across product, architecture, and skills.
- Related `.ai/` updates may ship with the code they describe.
- **Fix shape:** integrate into the real control flow (not a diagnosis bolt-on) and strip search debris before commit — see `AGENTS.md` § Commits → Before commit (fix).

### Pre-commit / pre-push gates

Do **not** commit or open/update a PR until these pass locally. `cargo check` alone is not enough for a commit — Clippy (`-D warnings`) is required **at commit / PR time**.

While iterating on a change, prefer `cargo check` (and targeted crate checks). Do **not** run the full Clippy gate on every edit — reserve it for immediately before commit or PR.

```sh
cargo fmt --all -- --check
cargo clippy -p fpmedia-web --all-targets --all-features -- -D warnings
cargo clippy --target wasm32-unknown-unknown -p fpmedia-web-demo-player -- -D warnings
cargo clippy --target wasm32-unknown-unknown -p fpmedia-web-components -- -D warnings
cargo clippy -p fpmedia-web-components --all-targets -- -D warnings
cargo check --target wasm32-unknown-unknown -p fpmedia-web-demo-player
cargo check --target wasm32-unknown-unknown -p fpmedia-web-components
cargo test -p fpmedia-web
cargo test -p fpmedia-web-components
(cd demo-player && trunk build)
```

- Always run `cargo fmt` (rustfmt) before committing Rust changes; include any fmt-only touch-ups in the same commit.
- Never commit code failing the gates above.
- For UI/player behaviour changes that e2e covers: also run `npm run e2e` with `trunk serve` (demo-player) on `:3000` when practical.

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
- **One crate per commit when possible** (`engine` / `fpmedia-web`, `components` / `fpmedia-web-components`, `demo-player`). Split demo UI bindings from engine fixes even if they land in the same session. Exception: a single atomic API change that must touch caller + callee together (then say so in the body). Related `.ai/` updates still ship with the crate they describe.
- Line counts exclude lockfiles and generated files when judging size.
