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

1. **Play dead after rapid scrub** (gop12 AVC + HEVC above) — transport Settling / decoder closed; repro `WEBMEDIA_CLIP=gop12 npm run e2e:stress`. Also `e2e/player.scrub.cold-open.spec.ts` Play-after.
2. **Decoding error / closed codec** under Exact/combo scrub (long-GOP, 60fps) — densify vs settle race.
3. **Slow resume on 60fps** (~10 s) after scrub release.
4. Native `FileSource` (`Seek`+`Read`, `cfg(not(wasm32))`) — replace `open_mp4_path` full `fs::read`.
5. no-Range → `ReadableStreamSource` (host full-download stays for now).
6. Drop `re_mp4` git pin when crates.io has [#35](https://github.com/rerun-io/re_mp4/pull/35).

### Later

- Pure audio path: `AudioPlayer` + `media_components::audio` (`AudioRef`) reusing `webmedia::audio` (mirror of `VideoPlayer`, thin — no canvas/scrub). Optional later: waveform chrome (not required for the element API).
- Hover thumbs optional: Web Worker + OffscreenCanvas if Play+thumb janks main-thread present.
- 4K loop/EOF residual: first-GOP warm after wrap — holds already 0; see `skills/media.md` “4K residual loop/EOF seam”. No committed 4K plate in testvideo yet.
- Broader packet / sample / canvas sink layering (decode pump+consumer shipped).
- Dual-source compare layout hooks.
- Publish `webmedia` / `media_components` crates for reuse by host apps.
- Production player chrome (demo controls in `app` stay for now).
