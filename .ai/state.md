# State

> In progress + Next up (+ Later) only. No Done list — finished items are deleted; durable lessons go to skills / product / architecture (`AGENTS.md` Documentation).

---

## Handoff (2026-09-25)

**Stable baseline:** closed-Range `UrlSource` + `UrlFetchGate` + `CachedSource` + sample-span coalesce. Domain SMs shipped through Session: transport, OpQueue, EngineMode, DecoderSlot, WorkloadMode, OpenPhase, ClockState, SessionModel. **Do not** re-land UrlSource half-ports without a full mediabunny cite.

**Failed experiments (skills gotchas — read before touching byte IO):**

1. Open-ended stream + abort/replace workers (no queue) → FetchMap holes / soft gray edges, cold-seek ~5 s, open hangs.
2. Closed-Range `ReadOrchestrator` (queue/merge/attach, no stream, no orch byte cache) → open warm ~10 s, holes/blur again, scrub-to-cold lag.

**Cite for next real port:** `mediabunny: src/source.ts` `ReadOrchestrator` + `_runWorker` — `queuedReads`, open-ended body, inner-slice resolve when bytes arrive. `CachedSource` alone ≠ that hole model.

**Commits to know (SM streak):** `7df7d0a` OpenPhase; `c9a64ef` ClockState; `1ba01e0` SessionModel. Earlier: `cad13db` WorkloadMode; `935c176` DecoderSlot; `a029ef4` EngineMode; `fcf4cf0` OpQueue; `238cffc` transport.

**Symptom still open:** gray scrubber strip on URL — slow / jumpy buffer (large blocks early, small near EOF is expected from quantum+`available`; holes/blur were from half-ports). Engine open/clock/session status typed; host `PlaySession` still separate.

**Paused until tomorrow:** full `ReadOrchestrator` port (pre-edit gate: cite + path match + no half-port).

---

## Next up

### Media engine (priority)

1. **Gray URL buffer** — only with **full** `ReadOrchestrator` port (`mediabunny: src/source.ts`): hole queue, max workers, open-ended stream **inside** workers, resolve inner read when bytes arrive. Until then keep gate + closed Range. Pre-edit gate in `AGENTS.md` (cite + path match) mandatory.
2. Native `FileSource` (`Seek`+`Read`, `cfg(not(wasm32))`) — replace `open_mp4_path` full `fs::read`.
3. no-Range → `ReadableStreamSource` (host full-download stays for now).
4. Drop `re_mp4` git pin `@aa5be9b` when crates.io has [#35](https://github.com/rerun-io/re_mp4/pull/35); QT `skip_box(0)` still worked around in `SparseBoxReader`.

### Later

- Pure audio path: `AudioPlayer` + `media_components::audio` (`AudioRef`) reusing `webmedia::audio` (mirror of `VideoPlayer`, thin — no canvas/scrub). Optional later: waveform chrome (not required for the element API).
- Hover thumbs optional: Web Worker + OffscreenCanvas if Play+thumb janks main-thread present.
- 4K loop/EOF residual: first-GOP warm after wrap — holds already 0; see `skills/media.md` “4K residual loop/EOF seam”. No committed 4K plate in testvideo yet.
- Broader packet / sample / canvas sink layering (decode pump+consumer shipped).
- Dual-source compare layout hooks.
- Publish `webmedia` / `media_components` crates for reuse by host apps.
- Production player chrome (demo controls in `app` stay for now).
