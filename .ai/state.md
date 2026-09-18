# State

> In progress + Next up (+ Later) only. No Done list — finished items are deleted; durable lessons go to skills / product / architecture (`AGENTS.md` Documentation).

## In progress

- Scrub policy by profile shipped: ShortFixed = max GOP ≤ 30 (scene-cut spread OK); `ScrubPolicy` soft-drag + exact-settle for Short/AllIntra; Exact-drag for Long; knobs on policy not globals. Re-probe ShotGrid with `WEBMEDIA_PERF_CLIP=…SHOTGRID.mp4` + `e2e/perf.scrub.cached.spec.ts` (expect status `GOP 30`, drag blit tracking).

---

## Next up

### Play / scrub (short path)

1. Attribute ShotGrid Next/Prev warm-window lag (~160–200 ms) after soft-drag policy — blit vs step path.
2. If blit window (96) still feels cold mid-timeline on Long — CSS-sized densify bitmaps or aim-centered blit budget (not full-clip 1080p RGBA); decide per profile after re-probe.
3. Optional: trim residual Play underrun (~0.96→1.0) at GOP boundaries if it shows up on longer clips.
4. Probes: `e2e/perf.scrub.cached.spec.ts` (+ ShotGrid via `WEBMEDIA_PERF_CLIP`); smoke; DRYP play.
5. Optional: contiguous densify coverage as timeline buffer bar.

### Media engine

- Re-save scrub baselines after deep-seek fix (`npm run e2e:baseline:save` then compare).
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
