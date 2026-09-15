# State

> In progress + Next up (+ Later) only. No Done list — finished items are deleted; durable lessons go to skills / product / architecture (`AGENTS.md` Documentation).

## In progress

- Wing It deep key→N (≥~60 frames from sync, e.g. frame 4110 / `slow-large`) still `flush/idle without pts` — cold settle @0.85 / seek≤~40 from key OK after settle cascade fixes.

---

## Next up

### Media engine

- Fix long key→N `flush/idle` on Wing It (threshold ~50–60 frames from key; `decodeQ≈18` after flush timeout). Settle one-cascade + windowed salvage + key-change recreate + chunk PTS assign landed (no dense trim on settle — wiped strip).
- Re-save scrub baselines after deep-seek fix (`npm run e2e:baseline:save` then compare).
- Filmstrip thumbs via `ThumbCache` (caller-sized bitmaps; mediabunny `samplesAtTimestamps` later).
- Spot-check A/V sync on long clips (Wing It / real AAC) after seek+play — mute/flush + volume UI shipped; clock already audio-driven when a track exists.
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
- Web Worker decode tier — only if profiling shows main-thread pump starving rAF after GOP profile + byte budgets.
