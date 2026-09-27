# State

> In progress + Next up (+ Later) only. No Done list — finished items are deleted; durable lessons go to skills / product / architecture (`AGENTS.md` Documentation).

---

## Handoff (2026-09-27)

**HEAD:** sticky-error clear (this commit; ahead of `origin/main`). Not pushed.

**Landed this session (scrub→Play / resume):**

- PlayPrime fill poison → `invalidate_decoder` + one refill; soft-park host (no bump).
- Settle: densify yield 250+250 ms; snap UI playhead→engine after settle; cold → await **one GOP** `warm_sample_bytes` → Exact+seed (not soft-cold → PlayPrime warm=24).
- Unprimed PlayPrime warm capped to GOP length (`gop_sample_lead` 8…24).
- Host: `error.set(None)` on **Ok** seek/scrub/step (`host_ops`); probe `e2e/player.scrub.sticky-error.spec.ts`.
- Probes: `e2e/player.scrub.cold-resume.spec.ts` (budget 4 s, passed); stress gop12 / bf2 / `2997_gop12` clean after fixes.

**Cite:** `mediabunny: examples/media-player/media-player.ts` `seekToTime` → `startVideoIterator`.

**Not done (do not assume fixed):**

- Fresh stress re-verify: **60fps** progressive (`gop12_60`, …) and **HEVC** gop12 / 2997 / 60.
- Exact/cold tap soft **Decoding error** / closed codec (matrix soft faults).
- First **Play on gray without prior Exact** — confirm GOP-cap warm (was fixed-24 lag).
- Full matrix refresh (`npm run e2e:stress:matrix`) — last report `e2e/stress-reports/` is **2026-09-26** (stale vs HEAD).

**Failed experiments (do not re-land):** open-ended abort/replace workers; closed-Range orch (no stream / no orch byte cache).

---

## Next up

### Media engine (priority)

1. **Re-verify 60fps + HEVC stress** — `WEBMEDIA_CLIP=gop12_60` / `hevc` / `hevc_1080p_2997_gop12_bf0` (`npm run e2e:stress`); resume + Play-dead budgets. Optionally refresh full matrix.
2. **Decoding error / closed codec** on Exact/cold taps (gop12/gop48 soft faults) — densify vs Exact race; abort put-back landed — still soft-faults.
3. **PlayPrime first Play on gray (no Exact)** — confirm GOP-lead warm is enough; add/adjust probe if still 1–3 s lag.
4. **Open UI lock until full gray** (progressive) — open warm lead=96 landed; confirm Ready before full gray.
5. Native `FileSource` (`Seek`+`Read`, `cfg(not(wasm32))`) — replace `open_mp4_path` full `fs::read`.
6. no-Range → `ReadableStreamSource` (host full-download stays for now).
7. Drop `re_mp4` git pin when crates.io has [#35](https://github.com/rerun-io/re_mp4/pull/35).

### Later

- Background play while tab hidden (`AudioContinue`: audio keeps running, skip paint, sync canvas on visible) — after Freeze; Web Audio may still suspend (not 1:1 with `<video>`). Default stays Freeze.
- Pure audio path: `AudioPlayer` + `media_components::audio` (`AudioRef`) reusing `webmedia::audio` (mirror of `VideoPlayer`, thin — no canvas/scrub). Optional later: waveform chrome (not required for the element API).
- Hover thumbs optional: Web Worker + OffscreenCanvas if Play+thumb janks main-thread present.
- 4K loop/EOF residual: first-GOP warm after wrap — holds already 0; see `skills/media.md` “4K residual loop/EOF seam”. No committed 4K plate in testvideo yet.
- Broader packet / sample / canvas sink layering (decode pump+consumer shipped).
- Dual-source compare layout hooks.
- Publish `webmedia` / `media_components` crates for reuse by host apps.
- Production player chrome (demo controls in `app` stay for now).
- Optional later: extract settle/prime budgets into `SettlePolicy` (after hard fails quiet) — inventory only until then.
