# State

> In progress + Next up (+ Later) only. No Done list — finished items are deleted; durable lessons go to skills / product / architecture (`AGENTS.md` Documentation).

## In progress

_(empty)_

---

## Next up

### Layout / crates

- `media_components::helpers` — shared UI math (cache progress segments, coalesced runs → CSS/%, time formatters). Keyboard chrome stays in demo `app`.

### Media engine

- Pure audio path: `AudioPlayer` + `media_components::audio` (`AudioRef`) reusing `webmedia::audio`.
- Progressive-open follow-ups: coalesce adjacent sample ranges; UrlSource parallel Range workers / sequential fallback (mediabunny orchestrator).
- Native `FileSource` (`Seek`+`Read`, `cfg(not(wasm32))`) — progressive open for host tests/CLI without loading whole file; replace `open_mp4_path`’s full `fs::read`.
- Optional: group visual decode under `webmedia::video` (mirror `audio/`; name is engine module, not the UI crate).
- Temporary deps: workspace pins `rerun-io/re_mp4` @ `aa5be9b` ([#35](https://github.com/rerun-io/re_mp4/pull/35) merged; not on crates.io yet).
- Upstream/re_mp4: nested `skip_box(0)` livelock on QT size-0 padding (we work around in `SparseBoxReader`).
- Drop `re_mp4` git pin when the next crates.io release includes #35.

### Later

- 4K loop/EOF residual: first-GOP warm after wrap — holds already 0; see `skills/media.md` “4K residual loop/EOF seam”. No committed 4K plate in testvideo yet.
- Broader packet / sample / canvas sink layering (decode pump+consumer shipped).
- Dual-source compare layout hooks.
- Publish `webmedia` / `media_components` crates for reuse by host apps.
- Production player chrome (demo controls in `app` stay for now).
