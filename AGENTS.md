# AGENTS.md — webmedia

Respect `.agentignore`.

## Start of session

1. Read `.ai/product.md` — domain + UX.
2. Read `.ai/architecture.md` — stack + code rules.
3. Read `.ai/state.md` — in progress + next up. No Done list.
4. Scan `.ai/skills/` filenames.
5. **Read the relevant skill file in full before touching code that imports its topic.**
6. **Media work:** for new features or bug fixes in demux / decode / seek / playback, consult [mediabunny](https://github.com/Vanilagy/mediabunny) first (docs + source), then port patterns into Rust — never add it as a dependency.

All `.ai/` documentation is English.

## Skills index

| File | Read before touching |
| ---- | -------------------- |
| `.ai/skills/media.md` | demux, frame index, player API, WebCodecs |
| `.ai/skills/leptos.md` | any Leptos UI / CSR mount / resources |

New gotcha → append to the relevant skill; add a skills-index row only if a new file is created.

## Documentation (`.ai/`)

| File | When to update | What goes there |
| ---- | -------------- | ---------------- |
| **`product.md`** | Domain or UX decision stabilises | Model, surfaces, business invariants. One row/paragraph per decision — no session notes. |
| **`architecture.md`** | Stack, layers, or DB policy stabilises | Layout, layer rules, code conventions, ADRs. Point to skills for how-to. |
| **`state.md`** | End of session; before commit if priorities shifted | **In progress** + **Next up** only (no Done list). Drop finished specs; don’t duplicate product/architecture. |
| **`skills/*.md`** | Reusable trap or API mismatch fixed in code | Triage, gotchas, patterns. Not backlog or layout. |

**Before commit:** include `.ai/` when architecture, player UI, or a skill-worthy fix changed. **No** long duplicate prose across product, architecture, and skills.

**End of session:** refresh `state.md`; append skill gotchas if any.

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
- Closes a state.md task → blank line then `closes: <task name>`.
- Never commit code failing `cargo check` or `cargo leptos build`.

## Dev commands

```sh
cargo leptos watch                                      # dev → http://127.0.0.1:3000
cargo leptos build                                      # build → target/site/
cargo check -p server                                   # server (ssr) type check
cargo check --target wasm32-unknown-unknown -p frontend # CSR WASM client + UI
cargo test -p webmedia                                  # media engine unit/integration tests
```
