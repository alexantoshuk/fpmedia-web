# State

> In progress + Next up (+ Later) only. No Done list — finished items are deleted; durable lessons go to skills / product / architecture (`AGENTS.md` Documentation).

---

## Handoff (2026-09-27)

**HEAD:** gray-first Play probe (this commit). Ahead of `origin/main` (check `git status`).

**Landed this session (scrub→Play / resume + sticky error + HoldHidden verify):**

- PlayPrime fill poison → `invalidate_decoder` + one refill; soft-park host (no bump).
- Settle: densify yield 250+250 ms; snap UI playhead→engine after settle; cold → await **one GOP** `warm_sample_bytes` → Exact+seed.
- Unprimed PlayPrime warm capped to GOP length (`gop_sample_lead` 8…24).
- Host: `error.set(None)` on **Ok** seek/scrub/step; probe `e2e/player.scrub.sticky-error.spec.ts`.
- `HoldHidden` ≠ underrun for bg sync-verify (`blocks_bg_sync_verify`).
- Stress re-verify (2026-09-27, HEAD): **pass** `gop12_60`, `hevc`, `hevc_1080p_2997_gop12_bf0`, `hevc_1080p_60_gop12_bf0` — budgets OK, `faults: []`. Soft console: densify re-abort / Decoding error / backpressure→soft-cold (esp. hevc 24).
- Gray/cold first Play (no Exact seed): probe `e2e/player.play.gray-first.spec.ts` **passed** (GOP warm lead / budget 2.5 s).

**Cite:** `mediabunny: examples/media-player/media-player.ts` `seekToTime` → `startVideoIterator`.

**Not done (do not assume fixed):**

- Exact/cold tap soft **Decoding error** / closed codec / backpressure — densify-busy→skip Exact landed; HEVC still flaky `Unsupported configuration` / closed codec on rare combo release+play (Chrome/Firefox often no HEVC; Edge OK on Windows).
- Full matrix refresh (`npm run e2e:stress:matrix`) — last report `e2e/stress-reports/` is **2026-09-26** (stale vs HEAD).

**Failed experiments (do not re-land):** open-ended abort/replace workers; closed-Range orch (no stream / no orch byte cache).

---

## Next up

### Media engine (priority)

1. **Decoding error / closed codec** (remaining) — densify-busy→skip Exact landed; HEVC still rare `Unsupported configuration` / closed codec on combo release+play. Probe/harden dual-HW recreate.
2. **Open UI lock until full gray** (progressive) — open warm lead=96 landed; confirm Ready before full gray.
3. Optional: refresh full matrix `npm run e2e:stress:matrix`.
4. Native `FileSource` (`Seek`+`Read`, `cfg(not(wasm32))`) — replace `open_mp4_path` full `fs::read`.
5. no-Range → `ReadableStreamSource` (host full-download stays for now).
6. Drop `re_mp4` git pin when crates.io has [#35](https://github.com/rerun-io/re_mp4/pull/35).

### Later

- Background play while tab hidden (`AudioContinue`: audio keeps running, skip paint, sync canvas on visible) — after Freeze; Web Audio may still suspend (not 1:1 with `<video>`). Default stays Freeze. Strip cache while hidden = best-effort / throttled (browser); not a hard fail.
- Pure audio path: `AudioPlayer` + `media_components::audio` (`AudioRef`) reusing `webmedia::audio` (mirror of `VideoPlayer`, thin — no canvas/scrub). Optional later: waveform chrome (not required for the element API).
- Hover thumbs optional: Web Worker + OffscreenCanvas if Play+thumb janks main-thread present.
- 4K loop/EOF residual: first-GOP warm after wrap — holds already 0; see `skills/media.md` “4K residual loop/EOF seam”. No committed 4K plate in testvideo yet.
- Broader packet / sample / canvas sink layering (decode pump+consumer shipped).
- Dual-source compare layout hooks.
- Publish `webmedia` / `media_components` crates for reuse by host apps.
- Production player chrome (demo controls in `app` stay for now).
- Optional later: extract settle/prime budgets into `SettlePolicy` (after hard fails quiet) — inventory only until then.
