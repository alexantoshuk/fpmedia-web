# State

## In progress

- **Progressive open (like `<video>` / mediabunny `BlobSource`)** — stop whole-file `array_buffer` into WASM.
  1. `MediaSource` + wasm `BlobSource` / native file source (~8 MiB cache).
  2. Ranged top-level box scan → `ftyp`+`moov` stub → `re_mp4` → `FrameIndex` (file offsets).
  3. Async sample fetch via `source.read` through WebCodecs decode/prefetch.
  4. `Video` / demo: `open_blob` instead of full-file buffer.
  5. Lazy AVC sync verify (off critical open path).
  6. Docs/tests: keep `open_mp4_bytes` for fixtures; offset-consistency check.
  - ADR sketch: `.ai/architecture.md` → Progressive open.
  - Temporary deps: workspace pins `alexantoshuk/re_mp4` @ `a6dceaa` until [rerun-io/re_mp4#35](https://github.com/rerun-io/re_mp4/pull/35) merges.

---

## Next up

### Media engine

- Audio track + shared A/V media clock (video slaves to audio once audio exists).
- HEVC bitstream key verify + AVC Recovery Point SEI (IDR-only verify is in place).
- After progressive open: coalesce adjacent sample ranges; optional background full sync-verify.

### Later

- Packet / sample / canvas sink layering (mediabunny mental model).
- Sparse `samplesAtTimestamps` for filmstrip thumbnails.
- Dual-source compare layout hooks.
- Publish `webmedia` / `video` crates for reuse by host apps.
- Production player chrome (demo controls in `app` stay for now).
- Drop `re_mp4` git pin when upstream #35 is released on crates.io.
