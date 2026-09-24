# State

> In progress + Next up (+ Later) only. No Done list — finished items are deleted; durable lessons go to skills / product / architecture (`AGENTS.md` Documentation).

## In progress

_(empty)_

---

## Next up

### Media engine

- Native `FileSource` (`Seek`+`Read`, `cfg(not(wasm32))`) — progressive open for host tests/CLI without loading whole file; replace `open_mp4_path`’s full `fs::read`.
- Progressive-open follow-ups: coalesce adjacent sample ranges; UrlSource open-ended Range streaming + mid-file sequential abandon (mediabunny `_runWorker` / `ReadableStreamSource` — gate shipped).
- Temporary deps: workspace pins `rerun-io/re_mp4` @ `aa5be9b` ([#35](https://github.com/rerun-io/re_mp4/pull/35) merged; not on crates.io yet).
- Upstream/re_mp4: nested `skip_box(0)` livelock on QT size-0 padding (we work around in `SparseBoxReader`).
- Drop `re_mp4` git pin when the next crates.io release includes #35.

### Later

- Pure audio path: `AudioPlayer` + `media_components::audio` (`AudioRef`) reusing `webmedia::audio` (mirror of `VideoPlayer`, thin — no canvas/scrub). Optional later: waveform chrome (not required for the element API).
- Hover thumbs optional: Web Worker + OffscreenCanvas if Play+thumb janks main-thread present.
- 4K loop/EOF residual: first-GOP warm after wrap — holds already 0; see `skills/media.md` “4K residual loop/EOF seam”. No committed 4K plate in testvideo yet.
- Broader packet / sample / canvas sink layering (decode pump+consumer shipped).
- Dual-source compare layout hooks.
- Publish `webmedia` / `media_components` crates for reuse by host apps.
- Production player chrome (demo controls in `app` stay for now).
