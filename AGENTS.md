# AGENTS.md — webmedia

Respect `.agentignore`.

## Start of session

1. Read `.ai/product.md` — domain + UX.
2. Read `.ai/architecture.md` — stack + code rules.
3. Read `.ai/state.md` — in progress + next up. No Done list.
4. Scan `.ai/skills/` filenames.
5. **Read the relevant skill file in full before touching code that imports its topic.**
6. **Media work (mandatory):** see [Mediabunny-first](#mediabunny-first) below.

All `.ai/` documentation is English.

## Mediabunny-first

For **any** media feature, bug fix, or behaviour change in demux / decode / seek / playback / sources / sinks:

1. **Open [Vanilagy/mediabunny](https://github.com/Vanilagy/mediabunny)** (guide + `src/`) and find the matching path **before** inventing a Rust approach.
2. **Port that pattern** into this codebase (names, control flow, wait/wake rules, edge-case guards).
3. **Do not** add mediabunny as a dependency — we reimplement in Rust/WASM.
4. Invent only when mediabunny has **no** equivalent; then note why in the relevant `.ai/skills/*.md` gotcha.
5. When a fix came from mediabunny, cite the file/symbol in the skill gotcha or commit body (`mediabunny: src/media-sink.ts queueDequeue`).

Rust/WASM specifics (Closures, `spawn_local`, no Promise ergonomics) are **adapters**, not a reason to skip the port. Most playback hangs here have been divergences from mediabunny wait/wake rules, not inherent Rust limits.

## Suboptimal proposals

If the user asks for a change that is **clearly less optimal** than the current design (double decode, worse latency, throwing away work already paid for, etc.):

1. **Warn first** — explain the cost and the better alternative in a short reply.
2. **Do not implement immediately** — wait for an explicit go-ahead after the warning.
3. Record durable trade-offs in the relevant `.ai/skills/*.md` gotcha (not only chat).

Example: dropping prune→dense salvage on settle “to avoid strip noise” re-decodes the whole GOP in densify — warn before removing it.

## Skills index

| File | Read before touching |
| ---- | -------------------- |
| `.ai/skills/media.md` | demux, frame index, player API, WebCodecs |
| `.ai/skills/leptos.md` | Leptos UI / `<Video>` / CSR mount / resources |

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
- **Always run `cargo fmt` (rustfmt) before committing** Rust changes; include any fmt-only touch-ups in the same commit.
- Never commit code failing `cargo check` or `cargo leptos build`.

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
- Related `.ai/` updates may ship with the code they describe.
- Line counts exclude lockfiles and generated files when judging size.

## Dev commands

```sh
cargo fmt                                               # rustfmt — required before every commit
cargo leptos watch                                      # dev → http://127.0.0.1:3000
cargo leptos build                                      # build → target/site/
cargo check -p server                                   # server (ssr) type check
cargo check --target wasm32-unknown-unknown -p frontend # CSR WASM client + UI
cargo check --target wasm32-unknown-unknown -p video    # Video element crate
cargo test -p webmedia                                  # media engine unit/integration tests
npm install && npm run e2e:install                      # once: Playwright (+ Chromium if CDN ok)
npm run e2e                                             # browser smoke (app up on :3000; Win uses system Chrome)
```
