# AGENTS.md — webmedia

Respect `.agentignore`.

## Portable docs (no editor lock-in)

Canonical project knowledge lives in **`AGENTS.md`** + **`.ai/`** (+ **`CONTRIBUTING.md`** for commit / PR gates). It must stay usable by any editor or agent (Cursor, Zed, Claude Code, etc.).

- **Do not** create or use a `.cursor/` tree (no `.cursor/rules`, no editor-only RULE.md mirrors).
- **Do not** duplicate `.ai/` content into editor-specific rule folders, Copilot instructions, or parallel “agent notes” files.
- Editor/IDE config that is not documentation (e.g. debug launch) is fine if it does not restate product/architecture/skills.
- New durable guidance → the table in [Documentation (`.ai/`)](#documentation-ai) / [One source of truth](#one-source-of-truth). One-line pointers elsewhere only.

## Start of session

1. Read `.ai/product.md` — domain + UX.
2. Read `.ai/architecture.md` — stack + code rules.
3. Read `.ai/state.md` — in progress + next up. No Done list.
4. Scan `.ai/skills/` filenames.
5. **Read the relevant skill before touching code that imports its topic** — start with its **Triage** table, then the sections it points to (read the full skill when the change is broad).
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

### Pre-edit gate (media) — hard stop

Before asking for approve-to-edit on media work, the plan **must** include:

1. **Cite** — `mediabunny: <path> <symbol>` for the behaviour being changed (or explicit **no equivalent** + why).
2. **Path match** — which mediabunny flow maps here (e.g. `samplesAtTimestamps` vs `samples()` / playback iterator vs single-timestamp seek).
3. **Defaults** — do not flip global decoder/sink knobs (`optimizeForLatency`, prune/close policy, max queue depth, PTS assign FIFO vs chunk timestamp) unless the cite shows mediabunny’s default or the same branch.

**Refuse to edit** (even after a vague `ok` on a knob-only plan) until that cite is in the plan. Probe-driven local hypotheses are fine for *diagnosis*; they do not authorize *implementation* without the port step.

Anti-pattern we already paid for: invent `drop PTS > target` / global `optimizeForLatency: false` to clear long-GOP `flush/idle`, then break play warm-ahead — mediabunny keeps consumer queue samples and leaves latency optional.

## Suboptimal proposals

If the user asks for a change that is **clearly less optimal** than the current design (double decode, worse latency, throwing away work already paid for, etc.):

1. **Warn first** — explain the cost and the better alternative in a short reply.
2. **Do not implement immediately** — wait for an explicit go-ahead after the warning.
3. Record durable trade-offs in the relevant `.ai/skills/*.md` gotcha (not only chat).

Example: dropping prune→dense salvage on settle “to avoid strip noise” re-decodes the whole GOP in densify — warn before removing it.

## Approve-before-edit

Before **any** write to the repo (code, config, docs, generated assets):

1. **Describe the plan first** — short: what, where, why (files/symbols if known).
2. **Media changes:** plan must satisfy [Pre-edit gate (media)](#pre-edit-gate-media--hard-stop) (mediabunny cite or explicit no-equivalent).
3. **Do not edit until the user explicitly consents** (e.g. `ok`, `go`, `да`, `approve`).
4. **Reading, search, and non-mutating checks** (`cargo check`, tests, `git status`/`diff`/`log`) are allowed without consent.
5. **Exception:** only if the user explicitly says to skip approval for that task or session.

If the plan changes mid-work (scope creep, different files), stop and re-approve before further edits.

## Skills index

| File | Read before touching |
| ---- | -------------------- |
| `.ai/skills/media.md` | demux, frame index, player API, WebCodecs |
| `.ai/skills/leptos.md` | Leptos UI / `<Video>` / CSR mount / resources |

New gotcha → append to the relevant skill; add a skills-index row only if a new file is created.
Do **not** create extra doc types (`glossary.md`, `history.md`, `decisions.md`, session logs) — extend the table above only when a new skill topic is real and recurring.

## Documentation (`.ai/`)

| File | When to update | What goes there |
| ---- | -------------- | ---------------- |
| **`product.md`** | Domain or UX decision stabilises | Model, surfaces, business invariants. One row/paragraph per decision — no session notes. |
| **`architecture.md`** | Stack, layers, or DB policy stabilises | Layout, layer rules, code conventions, ADRs. Point to skills for how-to. |
| **`state.md`** | End of session; before commit if priorities shifted | **In progress** + **Next up** only (no Done list). Drop finished specs; don’t duplicate product/architecture. |
| **`skills/*.md`** | Reusable trap or API mismatch fixed in code | Triage table + gotchas + patterns. Not backlog, workspace layout, or Dev commands. |

### One source of truth

| Topic | Canonical home | Elsewhere |
| ----- | -------------- | --------- |
| Mediabunny-first workflow | `AGENTS.md` | One-line pointer only |
| Approve-before-edit | `AGENTS.md` | Do not restate |
| Portable-docs / no `.cursor` | `AGENTS.md` | Do not restate |
| Product / UX invariants | `product.md` | Do not restate essays in skills |
| Crate layout, layers, ADRs | `architecture.md` | Skills may keep a **module map** for one crate |
| State machines over flag soup | `architecture.md` § State machines over flags | One-line pointer only — do not restate domain table in skills |
| Backlog / priorities | `state.md` | Never a Done list; never copy into skills |
| Engine traps / decode / scrub | `skills/media.md` | UI host points here for decode hangs |
| Leptos / `<Video>` host traps | `skills/leptos.md` | Engine skill points here for UI stuck |
| Dev commands | `AGENTS.md` | Do not duplicate in skills |
| Commits / PR size / pre-push gates | `CONTRIBUTING.md` | One-line pointer only — do not restate |
| Editor-specific agent rules | **nowhere** | Forbidden — use `.ai/` + this file |

If two files disagree, fix the canonical home and delete the duplicate prose.

### Skill hygiene

- Every skill starts with a short **Triage** table: symptom → section / rule (agents read this first).
- New gotcha format: **symptom → rule → symbol/file** (optional `mediabunny: src/…` cite). Prefer one tight bullet over a paragraph; expand only when the failure mode is subtle.
- When a gotcha grows into a design section (e.g. scrub caches), keep the design once under a named heading and link it from triage — do not paste the same policy into three bullets.
- After fixing a bug: update triage if the symptom is new; append/adjust the gotcha; remove obsolete claims (wrong clock, “planned” that shipped).
- E2E / perf probes that guard a failure mode: list them in that skill’s triage (paths under `e2e/`). Do not invent a separate testing doc until probes outgrow a handful of lines.

### State hygiene

- `state.md` = **In progress** + **Next up** (+ optional **Later**). Never **Done**.
- Finished work: delete the bullet (durable knowledge → skill/product/architecture).
- Keep Next up actionable and short; no session narrative.

**Before commit / PR push:** follow **[`CONTRIBUTING.md`](CONTRIBUTING.md)** (format, Clippy, checks, tests, size). Include `.ai/` when architecture, player UI, or a skill-worthy fix changed.

**End of session:** refresh `state.md` (drop finished items); append/trim skill triage + gotchas if any.

## Commits

Follow **[`CONTRIBUTING.md`](CONTRIBUTING.md)** for commit message format, size limits, and mandatory pre-commit / pre-push gates (including `cargo clippy … -- -D warnings`). Do not restate those rules here.

## Dev commands

Cheat sheet (mandatory **commit** gates + flags → [`CONTRIBUTING.md`](CONTRIBUTING.md)).

While coding: use `cargo check` / targeted crate checks. Run `cargo clippy … -- -D warnings` only immediately before commit or PR — not on every edit.

```sh
cargo fmt                                               # rustfmt
cargo leptos watch                                      # dev → http://127.0.0.1:3000
cargo leptos build                                      # build → target/site/
cargo check -p server                                   # server (ssr) type check
cargo check --target wasm32-unknown-unknown -p frontend # CSR WASM client + UI
cargo check --target wasm32-unknown-unknown -p media_components # <Video> element crate
# Pre-commit only (see CONTRIBUTING.md):
cargo clippy -p webmedia --all-targets --all-features -- -D warnings
cargo clippy --target wasm32-unknown-unknown -p frontend -- -D warnings
cargo clippy --target wasm32-unknown-unknown -p media_components -- -D warnings
cargo test -p webmedia                                  # media engine unit/integration tests
npm install && npm run e2e:install                      # once: Playwright (+ Chromium if CDN ok)
npm run e2e                                             # browser smoke (app up on :3000; Win uses system Chrome)
# Scrub perf probes: e2e/perf.scrub.*.spec.ts — listed in skills/media.md Triage
```
