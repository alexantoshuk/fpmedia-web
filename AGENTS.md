# AGENTS.md — webmedia

Respect `.agentignore`.

## Portable docs (no editor lock-in)

Canonical project knowledge lives in **`AGENTS.md`** + **`.ai/`** (+ **`CONTRIBUTING.md`** for commit / PR gates). It must stay usable by any editor or agent (Cursor, Zed, Claude Code, etc.).

- **Do not** create or use a `.cursor/` tree (no `.cursor/rules`, no editor-only RULE.md mirrors).
- **Do not** duplicate `.ai/` content into editor-specific rule folders, Copilot instructions, or parallel “agent notes” files.
- Editor/IDE config that is not documentation (e.g. debug launch) is fine if it does not restate product/architecture/skills.
- New durable guidance → [Documentation write gate](#documentation-write-gate--hard-stop) + [One source of truth](#one-source-of-truth). One-line pointers elsewhere only.

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
3. **`.ai/` / docs changes:** plan must satisfy [Documentation write gate](#documentation-write-gate--hard-stop) (canonical file + no duplicate prose).
4. **Do not edit until the user explicitly consents** (e.g. `ok`, `go`, `да`, `approve`).
5. **Reading, search, and non-mutating checks** (`cargo check`, tests, `git status`/`diff`/`log`) are allowed without consent.
6. **Exception:** only if the user explicitly says to skip approval for that task or session.
7. **Commits are separate** — edit/`go` approval is **not** commit consent. See [Commits](#commits).

If the plan changes mid-work (scope creep, different files), stop and re-approve before further edits.

## Skills index

| File | Read before touching |
| ---- | -------------------- |
| `.ai/skills/media.md` | demux, frame index, player API, WebCodecs, engine audio/source |
| `.ai/skills/leptos.md` | Leptos UI / `<Video>` / CSR mount / resources / host OpQueue |

New trap → follow [Documentation write gate](#documentation-write-gate--hard-stop) (usually triage row + tight gotcha). Add a skills-index row only if a **new** skill file is created.
Do **not** create extra doc types (`glossary.md`, `history.md`, `decisions.md`, session logs) — extend the table above only when a new skill topic is real and recurring.

## Documentation (`.ai/`)

| File | When to update | What goes there |
| ---- | -------------- | ---------------- |
| **`product.md`** | Domain or UX decision stabilises | Model, surfaces, business invariants. One row/paragraph per decision — no session notes, no store/policy knobs unless the user sees them. |
| **`architecture.md`** | Stack, layers, or policy stabilises | Layout, layer rules, code conventions, ADRs, design tables (caches / Play·Scrub·Settle policy). Point to skills for failure modes. **No backlog / Still-open lists** — those live in `state.md`. |
| **`state.md`** | End of session; **chat handoff**; before commit if priorities shifted | **In progress** + **Next up** (+ optional **Later**) only (no Done list). Drop finished specs; don’t duplicate product/architecture. |
| **`skills/*.md`** | Reusable trap or API mismatch fixed in code | Triage + tight gotchas (+ optional one-crate module map). Not backlog, design essays, Dev commands, or session history. |

### Documentation write gate — hard stop

Before editing **any** `.ai/` file (or restating project knowledge in `AGENTS.md` / `CONTRIBUTING.md`), the plan **must** name the **one** canonical file for each fact:

| Kind of fact | Canonical file |
| ------------ | -------------- |
| UX / product invariant | `product.md` |
| Stack, layers, ADR, policy/design table, FSM domain | `architecture.md` |
| Backlog / parked / “later” / next-up | `state.md` |
| Symptom → how not to re-break (engine) | `skills/media.md` |
| Symptom → how not to re-break (host/UI) | `skills/leptos.md` |
| Agent workflow / Mediabunny-first / approve / Dev cheat sheet | `AGENTS.md` |
| Commit format / Clippy gates / PR size | `CONTRIBUTING.md` |

**Hard rules:**

1. **One home** — the same fact must not appear as prose in two files. Everywhere else: **one-line pointer** (`see product.md § …` / `skills/media.md` triage).
2. **No essay append** — do not paste a session narrative into a skill “for safety”. Choose the row above; trim or move design upward.
3. **Disagree → fix canonical** — if two files conflict, edit the home and **delete** the duplicate; do not add a third explanation.
4. **Refuse doc edits** that only grow a skill/architecture without naming the canonical slot (same spirit as media pre-edit cite).

### One source of truth

| Topic | Canonical home | Elsewhere |
| ----- | -------------- | --------- |
| Mediabunny-first workflow | `AGENTS.md` | One-line pointer only |
| Approve-before-edit / ask-before-commit | `AGENTS.md` | Do not restate |
| Doc write gate / skill budgets | `AGENTS.md` | Do not restate |
| Portable-docs / no `.cursor` | `AGENTS.md` | Do not restate |
| Product / UX invariants | `product.md` | Do not restate essays in skills |
| Crate layout, layers, ADRs, cache/policy design | `architecture.md` | Skills: module map + traps only; pointer to design sections |
| State machines over flag soup | `architecture.md` § State machines over flags | One-line pointer only — do not restate domain table in skills |
| Backlog / Still open / priorities | `state.md` | Never a Done list; never in skills; never a Still-open section in architecture |
| Chat handoff → refresh `state.md` | `AGENTS.md` § State hygiene | One-line pointer only |
| Engine traps / decode / scrub / demux | `skills/media.md` | Host skill: one-line pointer only (no recipe copy) |
| Leptos / `<Video>` host traps | `skills/leptos.md` | Engine skill: one-line pointer only (no recipe copy) |
| Dev (iteration) commands / npm scripts | `AGENTS.md` § Dev commands | `README.md`: one-line pointer. Skills: **paths** under `e2e/` in triage Notes only — no command blocks |
| Commits / PR size / pre-push gates | `CONTRIBUTING.md` | One-line pointer only — do not restate |
| Editor-specific agent rules | **nowhere** | Forbidden — use `.ai/` + this file |

### Skill hygiene

- Every skill starts with a short **Triage** table: symptom → section / symbol (agents read this first).
- **Triage first:** new symptom → triage row; add a gotcha only if the failure mode is subtle (not already clear from the row).
- Gotcha format: **symptom → rule → symbol/file** (optional `mediabunny: src/…` cite). **Target ≤ ~8 lines** per gotcha. Longer = design → move to `architecture.md`, leave a link.
- **Do not** repeat the same policy in triage Notes **and** a gotcha **and** a design heading — one place + pointers.
- **Forbidden in skills:** `Was:` / `Superseded:` / changelog; backlog (`Future:`, parked); Dev/npm command blocks; product Exact/Play essays; architecture FSM/orch shipped essays; full recipes that belong in the other skill.
- **Probes:** triage Notes = `e2e/…` paths only (no `npm run …` lists).
- **Module map:** at most one per engine skill; do not restate workspace layout from `architecture.md`.
- **Soft size budgets** (trim/move before append): `skills/media.md` ≲ **200** lines; `skills/leptos.md` ≲ **80** lines. Oversized → cleanup in the same change, not “add a paragraph”.
- After a fix: update/trim triage; tighten or replace the gotcha; **delete** obsolete claims (wrong clock, “planned” that shipped, reverted half-ports).

### After-fix doc update

For a skill-worthy fix, the plan or commit must state **one** of:

- triage row added/updated; and/or gotcha tightened (not a new essay), or
- durable design → `architecture.md`, or
- UX decision → `product.md`, or
- finished work dropped from `state.md`

Do **not** only “append to `media.md`” without choosing a canonical slot.

### Product / architecture hygiene

- **`product.md`:** user-facing invariants. No decode-store / policy-field names unless the user observes them (strip colors, Exact vs Play, rate ladder).
- **`architecture.md`:** shipped ADRs say **shipped** (not “future” / “half-port”). No **Still open** bullet lists — pointer to `state.md`. Design tables live here once; skills link, they do not fork.

### State hygiene

- `state.md` = **In progress** + **Next up** (+ optional **Later**). Never **Done**.
- Finished work: delete the bullet (durable knowledge → skill/product/architecture).
- Keep Next up actionable and short; no session narrative; no paste of skill gotchas.
- Handoff claims must not contradict `architecture.md` / skills — if they disagree, fix the canonical home.
- **Chat handoff (hard):** when the user signals a move to another chat / session (e.g. «переходим в другой чат», «handoff», «new chat»), **refresh `.ai/state.md` before wrapping up** — accurate Handoff + Next up (what landed, what is unverified, what is still open). If skills were touched this session, **trim** obsolete/superseded skill lines in the same wrap-up. Do not leave a stale matrix or “re-verify” that already passed without saying so.

**Before commit / PR push:** follow **[`CONTRIBUTING.md`](CONTRIBUTING.md)** (format, Clippy, checks, tests, size). Include `.ai/` when architecture, player UI, or a skill-worthy fix changed — and keep that `.ai/` diff compliant with the write gate (no duplicate essays).

**End of session / chat handoff:** refresh `state.md` (drop finished items; update Handoff); trim skill triage + gotchas if any (do not grow skills “for the log”).

## Commits

**Ask first (hard):** never create a git commit unless the user explicitly asks (e.g. `commit`, `commi`, «закоммить», «сделай коммит»). Finishing a task, `go` / `ok` on an edit plan, or “impl …” is **not** consent to commit. Do not push unless asked.

When the user does ask to commit: follow **[`CONTRIBUTING.md`](CONTRIBUTING.md)** for message format, size limits, and mandatory pre-commit / pre-push gates (including `cargo clippy … -- -D warnings`). Do not restate those rules here.

## Dev commands

Iteration cheat sheet. Mandatory **commit / PR** gates (Clippy, fmt check, build) live only in [`CONTRIBUTING.md`](CONTRIBUTING.md) — run them immediately before commit or PR, not on every edit.

```sh
cargo fmt                                               # rustfmt
cargo leptos watch                                      # dev → http://127.0.0.1:3000
cargo leptos build                                      # build → target/site/
cargo check -p server                                   # server (ssr) type check
cargo check --target wasm32-unknown-unknown -p frontend # CSR WASM client + UI
cargo check --target wasm32-unknown-unknown -p media_components # <Video> element crate
cargo check --target wasm32-unknown-unknown -p webmedia --features webcodecs # engine wasm-only code
# Opt-in HEVC: `cargo test -p webmedia --features hevc`; demo WASM →
# `media_components` feature `hevc` (e.g. default = ["thumbs", "hevc"] while testing)
cargo test -p webmedia                                  # engine tests (native; wasm-only modules not covered)
cargo test -p media_components                          # host UI math (native; wasm modules cfg'd out)
npm install && npm run e2e:install                      # once: Playwright (+ Chromium if CDN ok)
npm run e2e                                             # browser smoke (app up on :3000; Win uses system Chrome)
# Scrub perf probes: e2e/perf.scrub.*.spec.ts — listed in skills/media.md Triage
```
