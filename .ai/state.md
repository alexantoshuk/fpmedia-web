# State

> In progress + Next up (+ Later) only. No Done list — finished items are deleted; durable lessons go to skills / product / architecture (`AGENTS.md` Documentation).

## In progress

- Spot-check BBB Play across GOP boundaries after soft sequential key switch (encode size = viewport ≤ source×monitor).
- Play Exact trail: sequential ingest of presented frames (not gap densify) — plan below.

---

## Next up

### Play Exact trail (presented-only ingest)

**Goal:** while playing, record frames already shown into `ScrubChunkStore` (WebP), same frame/byte budget + farthest-from-playhead eviction. Not a mirror of scrub densify.

**Non-goals:** gap-fill / scrub-decoder densify during Play (stays off); every-frame main-thread stall; ThumbCache/filmstrip (separate).

**Plan:**

1. **`present_play_frame`** — after canvas draw, ingest VF → WebP (`put_chunk` / replace) then drop VF; keep aim/playhead eviction. Do **not** call `schedule_scrub_cache_fill` / scrub decoder.
2. **Cost control (v1)** — sync encode at WebP **q=0.9** (already shipped); if Play janks, gate ingest (every Nth frame and/or spare-time before next deadline). Close VF immediately after encode (no long-lived play VF trail).
3. **v2 (optional)** — post-present copy/transfer → Worker WebP → store; abort backlog on underrun. Only if v1 profiling shows main-thread encode starving rAF.
4. **Probes** — smoke: Play grows Exact marks along playhead; Play still smooth (`player.play.dryp`); Pause densify unchanged; no densify task while `CacheFillMode::Play`.
5. **Docs** — `product.md` play-vs-scrub bullet; `skills/media.md` triage/gotcha (`present_play_frame` ingest ≠ densify). `mediabunny:` no Exact WebP trail — local invention.

### Play / scrub cache split (finish)

1. Run / fix probes: `e2e/player.smoke.spec.ts`; `e2e/player.play.dryp.spec.ts`; `e2e/perf.scrub.cached.spec.ts`; scrub settle if Exact blit regresses.
2. Byte-budget eviction for compressed chunks (frame-count budget still used).
3. Filmstrip via `ThumbCache` (caller-sized; mediabunny `samplesAtTimestamps` later).

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
- Web Worker encode/decode tier — play Exact ingest v2 and/or densify off main; only if profiling shows main-thread pump/encode starving rAF after GOP profile + byte budgets.
