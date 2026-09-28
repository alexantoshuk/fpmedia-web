# State

> In progress + Next up (+ Later) only. No Done list — finished items are deleted; durable lessons go to skills / product / architecture (`AGENTS.md` Documentation).

---

## Handoff (2026-09-28)

**HEAD:** local (SettlePolicy extract). Ahead of `origin/main` (check `git status`).

**Landed this session (policy hygiene):**

- `SettlePolicy` + consts in `types` — densify yield, gop lead clamp, play prime/index lead, take_decoder wait; `VideoMetadata::settle_policy()` (clip-invariant Default).
- Wired scrub densify yield / `gop_sample_lead`, PlayPrime warm, prefetch lead, `take_decoder`; demux open warm shares `SETTLE_PLAY_INDEX_LEAD`.
- Soft-cold / poison documented as **named recovery** (not knobs). Dropped dead `LOOP_PRIME_*` Hd aliases.
- **Cite:** mediabunny **no equivalent** settle timers — `seekToTime` → iterator `return` + `startVideoIterator`.

**Prior still holds:** PlayPrime poison recreate, settle yield+gop warm, sticky error clear, HoldHidden ≠ underrun, gray-first Play, HEVC feature-off default. AVC stress 15/15 (2026-09-28).

**Not done:** Exact/cold residual soft Decoding error when `hevc` on; OpenIoDefaults extraction (not needed yet).

**Failed experiments (do not re-land):** open-ended abort/replace workers; closed-Range orch (no stream / no orch byte cache).

---

## Next up

### Media engine (priority)

1. Native `FileSource` (`Seek`+`Read`, `cfg(not(wasm32))`) — replace `open_mp4_path` full `fs::read`.
2. no-Range → `ReadableStreamSource` (host full-download stays for now).
3. Drop `re_mp4` git pin when crates.io has [#35](https://github.com/rerun-io/re_mp4/pull/35).
4. Later / opt-in: enable cargo feature `hevc` + harden dual-HW if shipping H.265 again (+ `e2e:stress:matrix:hevc`).

### Later

- Background play while tab hidden (`AudioContinue`: audio keeps running, skip paint, sync canvas on visible) — after Freeze; Web Audio may still suspend (not 1:1 with `<video>`). Default stays Freeze. Strip cache while hidden = best-effort / throttled (browser); not a hard fail.
- Pure audio path: `AudioPlayer` + `media_components::audio` (`AudioRef`) reusing `webmedia::audio` (mirror of `VideoPlayer`, thin — no canvas/scrub). Optional later: waveform chrome (not required for the element API).
- Hover thumbs optional: Web Worker + OffscreenCanvas if Play+thumb janks main-thread present.
- 4K loop/EOF residual: first-GOP warm after wrap — holds already 0; see `skills/media.md` “4K residual loop/EOF seam”. No committed 4K plate in testvideo yet.
- Broader packet / sample / canvas sink layering (decode pump+consumer shipped).
- Dual-source compare layout hooks.
- Publish `webmedia` / `media_components` crates for reuse by host apps.
- Production player chrome (demo controls in `app` stay for now).
- Optional later: branch `SettlePolicy` by GOP/resolution only if probes demand (defaults shipped clip-invariant).
