# State

> In progress + Next up (+ Later) only. No Done list — finished items are deleted; durable lessons go to skills / product / architecture (`AGENTS.md` Documentation).

## In progress

- (idle)

---

## Next up

### Play / scrub (short path)

_(empty)_

### Media engine

- Re-save scrub baselines after deep-seek fix (`npm run e2e:baseline:save` then compare).
- Spot-check A/V sync on long clips (Wing It / real AAC) after seek+play — include review rates and JKL `2×`/`4×` (audio on); `8×` stays mute.
- Pure audio path later: `AudioPlayer` / `AudioRef` (no canvas) reusing demux + decode + clock.
- HEVC bitstream key verify (AVC IDR + Recovery Point SEI done).
- Progressive-open follow-ups: coalesce adjacent sample ranges; optional background / lazy AVC sync-verify for huge files if open latency matters.
- Native `FileSource` (`Seek`+`Read`, `cfg(not(wasm32))`) — progressive open for host tests/CLI without loading whole file; replace `open_mp4_path`’s full `fs::read`.
- Temporary deps: workspace pins `rerun-io/re_mp4` @ `aa5be9b` ([#35](https://github.com/rerun-io/re_mp4/pull/35) merged; not on crates.io yet).
- Upstream/re_mp4: nested `skip_box(0)` livelock on QT size-0 padding (we work around in `SparseBoxReader`).
- Drop `re_mp4` git pin when the next crates.io release includes #35.

### Later

- Broader packet / sample / canvas sink layering (decode pump+consumer shipped).
- Dual-source compare layout hooks.
- Publish `webmedia` / `video` crates for reuse by host apps.
- Production player chrome (demo controls in `app` stay for now).
