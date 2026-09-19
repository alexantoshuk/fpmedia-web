# State

> In progress + Next up (+ Later) only. No Done list — finished items are deleted; durable lessons go to skills / product / architecture (`AGENTS.md` Documentation).

## In progress

- (idle)

---

## Next up

### Play / scrub (short path)

1. Optional: contiguous densify coverage as a dedicated timeline buffer bar (strip already Covered+Exact gradients).
2. Spot-check Wing It mid-timeline Next/Prev after blit MiB (windowed vs fullscreen Exact width).
3. Frame.io-oriented playback rate (≈0.25–1.75 / JKL); reverse/J later — not YouTube-style.

### Media engine

- Re-save scrub baselines after deep-seek fix (`npm run e2e:baseline:save` then compare).
- Spot-check A/V sync on long clips (Wing It / real AAC) after seek+play.
- Pure audio path later: `AudioPlayer` / `AudioRef` (no canvas) reusing demux + decode + clock.
- HEVC bitstream key verify (AVC IDR + Recovery Point SEI done).
- Progressive-open follow-ups: coalesce adjacent sample ranges; optional background / lazy AVC sync-verify for huge files if open latency matters.
- Native `FileSource` (`Seek`+`Read`, `cfg(not(wasm32))`) — progressive open for host tests/CLI without loading whole file; replace `open_mp4_path`’s full `fs::read`.
- Temporary deps: workspace pins `alexantoshuk/re_mp4` @ `cd63eca` until [rerun-io/re_mp4#35](https://github.com/rerun-io/re_mp4/pull/35) merges.
- Upstream/re_mp4: nested `skip_box(0)` livelock on QT size-0 padding (we work around in `SparseBoxReader`).

### Later

- Broader packet / sample / canvas sink layering (decode pump+consumer shipped).
- Dual-source compare layout hooks.
- Publish `webmedia` / `video` crates for reuse by host apps.
- Production player chrome (demo controls in `app` stay for now).
- Drop `re_mp4` git pin when upstream #35 is released on crates.io.
