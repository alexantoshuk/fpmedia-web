# State

## In progress

(none)

---

## Next up

### Scrub / settle perf (Wing It follow-ups)

Done: densify abort + seekToKey→seekTo on settle (~4.5s exact; was eng stuck at 0 / tab freeze). Probe: `e2e/perf.scrub.settle.spec.ts`.
Done: settle drain / backpressure — same-pass prune-close, `held+decodeQ` vs `computeMaxQueueSize`, shallow seek-prune cap, `onQueueDequeue` wakes, idle main decoder before densify after exact (`mediabunny: src/media-sink.ts queueDequeue` / `computeMaxQueueSize`).
Done: long-GOP settle interim — static best cached/key on canvas; warm seekTo salvages prune→dense (no blit) so densify after exact is cheap. Blit interim = fake play; dropping salvage = double decode (see `AGENTS.md` Suboptimal proposals).
Done: drag densify vs settle — cancel checks inside `await_decode_capacity` / skip 2s `flush` on abort; fill task exits immediately on hold (probe: `e2e/perf.scrub.drag-settle.spec.ts`).
Done: AVC Recovery Point SEI in `verifyKeyPackets` — exact SEI (`recovery_frame_cnt==0 && exact_match`) kept as sync except Chromium &lt;145 (`mediabunny: src/codec-data.ts determineVideoPacketType`; w3c/webcodecs#650). Wing It lying syncs demoted on old Chrome; kept on modern Blink / FF / Safari.

### Media engine

- Sync-index binary search (`nearest_keyframe` / `gop_frame_ids` O(log n)).
- Filmstrip thumbs via `ThumbCache` (caller-sized bitmaps; mediabunny `samplesAtTimestamps` later).
- Audio track + shared A/V media clock (video slaves to audio once audio exists). Wall-clock play works until then.
- HEVC bitstream key verify (AVC IDR + Recovery Point SEI done).
- After progressive open: coalesce adjacent sample ranges; optional background sync-verify for huge files if open latency matters.
- Native `FileSource` (`Seek`+`Read`, `cfg(not(wasm32))`) — progressive open for host tests/CLI without loading whole file; replace `open_mp4_path`’s full `fs::read`.
- Temporary deps: workspace pins `alexantoshuk/re_mp4` @ `cd63eca` until [rerun-io/re_mp4#35](https://github.com/rerun-io/re_mp4/pull/35) merges.
- Upstream/re_mp4: nested `skip_box(0)` livelock on QT size-0 padding (we work around in `SparseBoxReader`).

### Later

- Packet / sample / canvas sink layering (mediabunny mental model).
- Dual-source compare layout hooks.
- Publish `webmedia` / `video` crates for reuse by host apps.
- Production player chrome (demo controls in `app` stay for now).
- Drop `re_mp4` git pin when upstream #35 is released on crates.io.
- Web Worker decode tier — only if profiling shows main-thread pump starving rAF after GOP profile + byte budgets.
