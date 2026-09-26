# State

> In progress + Next up (+ Later) only. No Done list — finished items are deleted; durable lessons go to skills / product / architecture (`AGENTS.md` Documentation).

---

## Handoff (2026-09-26)

**Stable baseline:** `UrlSource` + full `ReadOrchestrator` + open-ended `_runWorker` + `CachedSource` + sample-span coalesce. Stress e2e: fast open→zone scrub→resume (`npm run e2e:stress`); full Release matrix `npm run e2e:stress:matrix` → `e2e/stress-reports/` (gitignored).

**Cite:** `mediabunny: src/source.ts` `ReadOrchestrator` + `UrlSource._runWorker`.

**Matrix result (18 clips, fast stress):** 14 passed / 4 failed. Reports: `e2e/stress-reports/summary.md`.

**Hard fail — Play did not start after rapid scrub:**

- `avc_1080p_24_gop12_bf0`, `avc_1080p_24_gop12_bf2`
- `hevc_1080p_24_gop12_bf0`, `hevc_1080p_2997_gop12_bf0`

**Soft faults on passed runs:** `Decoding error` / closed codec on Exact/combo (gop48, 2997_gop*, 60fps); 60fps resume often ~9–10 s (budget edge).

**Failed experiments (do not re-land):** open-ended abort/replace workers; closed-Range orch (no stream / no orch byte cache).

---

## Next up

### Media engine (priority) — bugfix from stress matrix

1. **Open UI lock until full gray** (progressive) — open warm lead=96 landed; confirm Ready before full gray.
2. **Play dead after rapid scrub** — still fails on some gop12 / 2997 / 60fps / HEVC stress clips (`no resume` / `Play did not start`); soft-park landed — re-verify.
3. **Decoding error / closed codec** — cold/exact taps still fault on gop12 + gop48 Exact; abort put-back landed — re-verify stress.
4. **PlayPrime lag on gray (no prior Exact)** — cold path still awaits warm=24; verify first Play on gray without scrub.
5. **Slow resume on 60fps** (~10 s) after scrub release — stress: 60fps clips often miss Pause→Play sample / slow scrub→Play.
6. Native `FileSource` (`Seek`+`Read`, `cfg(not(wasm32))`) — replace `open_mp4_path` full `fs::read`.
7. no-Range → `ReadableStreamSource` (host full-download stays for now).
8. Drop `re_mp4` git pin when crates.io has [#35](https://github.com/rerun-io/re_mp4/pull/35).

**Stress matrix (2026-09-26, fast + Pause/scrub→Play budgets):** 8 passed / 10 failed — see `e2e/stress-reports/summary.md`. Pause→Play / Exact scrub→Play usually <600 ms when sample exists; fails are mostly decode poison / no resume / 60fps.

### Later

- Background play while tab hidden (`AudioContinue`: audio keeps running, skip paint, sync canvas on visible) — after Freeze; Web Audio may still suspend (not 1:1 with `<video>`). Default stays Freeze.
- Pure audio path: `AudioPlayer` + `media_components::audio` (`AudioRef`) reusing `webmedia::audio` (mirror of `VideoPlayer`, thin — no canvas/scrub). Optional later: waveform chrome (not required for the element API).
- Hover thumbs optional: Web Worker + OffscreenCanvas if Play+thumb janks main-thread present.
- 4K loop/EOF residual: first-GOP warm after wrap — holds already 0; see `skills/media.md` “4K residual loop/EOF seam”. No committed 4K plate in testvideo yet.
- Broader packet / sample / canvas sink layering (decode pump+consumer shipped).
- Dual-source compare layout hooks.
- Publish `webmedia` / `media_components` crates for reuse by host apps.
- Production player chrome (demo controls in `app` stay for now).
