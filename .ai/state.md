# State

> In progress + Next up (+ Later) only. No Done list — finished items are deleted; durable lessons go to skills / product / architecture (`AGENTS.md` Documentation).

## In progress

- Re-save scrub baselines for testvideo GOP12 + GOP48 (`npm run e2e:baseline:save` with app on :3000).

---

## Next up

### Play / scrub (short path)

_(empty)_

### Media engine

- Pure audio path later: `AudioPlayer` / `AudioRef` (no canvas) reusing demux + decode + clock.
- HEVC bitstream key verify (AVC IDR + Recovery Point SEI done); plate: testvideo `hevc_1080p_24_gop12_bf0`.
- Progressive-open follow-ups: coalesce adjacent sample ranges; optional background / lazy AVC sync-verify for huge files if open latency matters; UrlSource parallel Range workers / sequential fallback (mediabunny orchestrator).
- Native `FileSource` (`Seek`+`Read`, `cfg(not(wasm32))`) — progressive open for host tests/CLI without loading whole file; replace `open_mp4_path`’s full `fs::read`.
- Temporary deps: workspace pins `rerun-io/re_mp4` @ `aa5be9b` ([#35](https://github.com/rerun-io/re_mp4/pull/35) merged; not on crates.io yet).
- Upstream/re_mp4: nested `skip_box(0)` livelock on QT size-0 padding (we work around in `SparseBoxReader`).
- Drop `re_mp4` git pin when the next crates.io release includes #35.

### Later

- 4K loop/EOF residual: first-GOP warm after wrap — holds already 0; see `skills/media.md` “4K residual loop/EOF seam”. No committed 4K plate in testvideo yet.
- Broader packet / sample / canvas sink layering (decode pump+consumer shipped).
- Dual-source compare layout hooks.
- Publish `webmedia` / `video` crates for reuse by host apps.
- Production player chrome (demo controls in `app` stay for now).
