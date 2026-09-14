# State

## In progress

- Wing It cold scrub settle: exact paints in ~4.5s after densify-abort + seekToKey→seekTo fix (was eng stuck at 0 / tab freeze).

---

## Next up

### Media engine

- Sync-index binary search (`nearest_keyframe` / `gop_frame_ids` O(log n)).
- Filmstrip thumbs via `ThumbCache` (caller-sized bitmaps; mediabunny `samplesAtTimestamps` later).
- Audio track + shared A/V media clock (video slaves to audio once audio exists). Wall-clock play works until then.
- HEVC bitstream key verify + AVC Recovery Point SEI (IDR-only verify is in place).
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
