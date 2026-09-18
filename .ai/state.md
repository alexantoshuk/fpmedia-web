# State

> In progress + Next up (+ Later) only. No Done list — finished items are deleted; durable lessons go to skills / product / architecture (`AGENTS.md` Documentation).

## In progress

- ShotGrid proxy probes done (release `:3000`); next: attribute/fix bottlenecks **for short-IP profile** (clip currently labeled `GopProfile::Long` `GOP ~27 (max 30)` because scene-cut spread > 2).

**Probe tag — ShotGrid Wing It proxy** (`WEBMEDIA_PERF_CLIP=…SHOTGRID.mp4`, 1080p24 AVC, ~GOP30 IP):

| Metric | Result | Note |
| ------ | ------ | ---- |
| open | ~0.9 s | ok |
| densify warm start | ~60f in ≪1 s | cheap on this encode |
| cold/large settle | ~0.3–1.0 s | exact ok for short GOP |
| Next/Prev in warm win | p50 ~160 / ~200 ms | not blit-instant — investigate step path |
| drag pass1 (no wait) | **engineChanges: 0** | canvas/engine not tracking drag — main feel gap vs ShotGrid |
| drag pass2 (wait≤500 ms) | move→eng p50 ~82 ms | cached reverse better, still soft |

Do **not** raise global `SCRUB_BLIT_LRU` from this alone — prefer profile policy (soft-drag + ShortFixed/short-IP bucket).

---

## Next up

### Play / scrub (short path)

1. **Scrub policy by profile** (mediabunny: no equivalent) — on open, classify (`GopProfile` + B-frames / resolution class); apply `ScrubPolicy` (soft-drag vs exact-drag, densify on/off, blit window / marker budget, hard vs soft key recreate). Soft-drag + exact-settle for ShotGrid-like short IP; keep exact-primary for Long. Tune knobs **into the matching profile**, not global constants. Widen `ShortFixed` / add short-IP bucket if GOP≈30 with scene-cut spread currently falls into `Long`.
2. If blit window (96) still feels cold mid-timeline on Long — CSS-sized densify bitmaps or aim-centered blit budget (not full-clip 1080p RGBA); decide per profile after ShotGrid probe.
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
