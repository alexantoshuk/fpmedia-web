# State

## In progress

- (none — progressive Blob open landed; see Next up)

---

## Next up

### Media engine

- Audio track + shared A/V media clock (video slaves to audio once audio exists). Wall-clock play works until then.
- HEVC bitstream key verify + AVC Recovery Point SEI (IDR-only verify is in place).
- After progressive open: coalesce adjacent sample ranges; optional background sync-verify for huge files if open latency matters.
- Native `FileSource` (`Seek`+`Read`, `cfg(not(wasm32))`) — progressive open for host tests/CLI without loading whole file; replace `open_mp4_path`’s full `fs::read`.
- Temporary deps: workspace pins `alexantoshuk/re_mp4` @ `a6dceaa` until [rerun-io/re_mp4#35](https://github.com/rerun-io/re_mp4/pull/35) merges.

### Later

- Packet / sample / canvas sink layering (mediabunny mental model).
- Sparse `samplesAtTimestamps` for filmstrip thumbnails.
- Dual-source compare layout hooks.
- Publish `webmedia` / `video` crates for reuse by host apps.
- Production player chrome (demo controls in `app` stay for now).
- Drop `re_mp4` git pin when upstream #35 is released on crates.io.
