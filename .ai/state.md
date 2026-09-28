# State

> In progress + Next up (+ Later) only. No Done list — finished items are deleted; durable lessons go to skills / product / architecture (`AGENTS.md` Documentation).

---

## Handoff (2026-09-28)

**HEAD:** local — SettlePolicy + cold Seek Exact fix. Ahead of `origin/main` (check `git status`).

**Landed:**

- `SettlePolicy` (clip-invariant densify yield / gop lead / PlayPrime / take_decoder).
- Cold `seek_frame`: `ensure_indexed` + GOP warm + Exact (soft-cold only if densify busy); densify arm at pending aim. e2e: single seek (no blur+Seek), JKL wait idle/play, mute assert accessible name.
- Probes: fmp4-gap, JKL×3, mute smoke **pass**.

**Cite:** mediabunny `seekToTime` → retrieve then `startVideoIterator` (not soft-cold-only).

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
