# State

> In progress + Next up (+ Later) only. No Done list — finished items are deleted; durable lessons go to skills / product / architecture (`AGENTS.md` Documentation).

---

## Handoff (2026-09-25)

**Stable baseline:** `UrlSource` + full `ReadOrchestrator` (`source/orchestrator.rs`) + open-ended `_runWorker` stream + `CachedSource` + sample-span coalesce. Domain SMs through Session remain. Orch cache = mediabunny 64 MiB; `CachedSource` 512 MiB + FetchMap for gray strip.

**Cite:** `mediabunny: src/source.ts` `ReadOrchestrator` + `UrlSource._runWorker` — `queuedReads`, open-ended body, inner-slice resolve when bytes arrive.

**Verify next:** cold rapid-tap scrub + Play after (`e2e/player.scrub.cold-open.spec.ts`); URL gray strip holes/soft edges.

**Failed experiments (kept as skill gotchas — do not re-land):**

1. Open-ended stream + abort/replace workers (no queue).
2. Closed-Range `ReadOrchestrator` (queue/merge/attach, no stream, no orch byte cache).

---

## Next up

### Media engine (priority)

1. **Verify** cold rapid-tap scrub + Play-after (`e2e/player.scrub.cold-open.spec.ts`) + gray URL buffer after orch port.
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
