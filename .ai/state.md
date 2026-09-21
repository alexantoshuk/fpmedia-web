# State

> In progress + Next up (+ Later) only. No Done list — finished items are deleted; durable lessons go to skills / product / architecture (`AGENTS.md` Documentation).

## In progress

_(empty)_

---

## Next up

### Hover thumbs (priority)

Shipped: soft-drag Exact-blit only + sync playhead; hover **read-only** (ThumbCache / Exact downscale — no densify decode). Demo/engine still gate hover with `!playing && !scrubbing`. Outside Exact, hover stays blank.

Ordered plan:

1. **Compile + runtime gates** — cargo feature `thumbs` (**default on**) on `webmedia` / `media_components` / demo; runtime `ThumbPolicy::Off | On` (`On` = play **and** pause; host hides only while scrubbing). Lazy `Option<ThumbPreview>`; `Off` drops sink. Soft stubs when feature off so hosts keep compiling.
2. **Hover while playing** — drop engine/demo `!playing` gates; cancel hover only on scrub start / leave (not on Play). Exact still trimmed on Play → often empty until (3).
3. **Dedicated thumb decode** (mediabunny: separate small `CanvasSink` / `canvasesAtTimestamps`) — long-lived **3rd** decoder → **ThumbCache only**; never ingest Covered/Exact; do not steal scrub densify slot. Prefer software / small config for HEVC (avoid dual-HW stall). Coalesce latest-wins; no create-per-move.
4. **Optional later** — Web Worker + OffscreenCanvas if Play+thumb janks main-thread present; not required for MVP.

### Media engine

- Pure audio path: `AudioPlayer` + `media_components::audio` (`AudioRef`) reusing `webmedia::audio` (mirror of `VideoPlayer`, thin — no canvas/scrub).
- Progressive-open follow-ups: coalesce adjacent sample ranges; UrlSource parallel Range workers / sequential fallback (mediabunny orchestrator).
- Split `video_player/` into play/scrub/seek modules (orchestration only; `scrub_blit` stays in `decode/`).
- Native `FileSource` (`Seek`+`Read`, `cfg(not(wasm32))`) — progressive open for host tests/CLI without loading whole file; replace `open_mp4_path`’s full `fs::read`.
- Temporary deps: workspace pins `rerun-io/re_mp4` @ `aa5be9b` ([#35](https://github.com/rerun-io/re_mp4/pull/35) merged; not on crates.io yet).
- Upstream/re_mp4: nested `skip_box(0)` livelock on QT size-0 padding (we work around in `SparseBoxReader`).
- Drop `re_mp4` git pin when the next crates.io release includes #35.

### Later

- 4K loop/EOF residual: first-GOP warm after wrap — holds already 0; see `skills/media.md` “4K residual loop/EOF seam”. No committed 4K plate in testvideo yet.
- Broader packet / sample / canvas sink layering (decode pump+consumer shipped).
- Dual-source compare layout hooks.
- Publish `webmedia` / `media_components` crates for reuse by host apps.
- Production player chrome (demo controls in `app` stay for now).
