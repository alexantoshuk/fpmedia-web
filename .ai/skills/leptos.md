# Leptos 0.8.x (`fpmedia-web-components` / `demo-player`)

> Read before touching components, resources, or routing. Pins **Leptos 0.8.x**.
> Engine decode/scrub hangs → `skills/media.md`. Stack/CSR/leptos-use → `architecture.md`.

## Triage

| Symptom | Look first |
| ------- | ---------- |
| Android Browse / file picker dead | `.wm-file input` not `display:none` — opacity overlay |
| Open stuck after `open_blob start` | `wasm_bindgen_futures::spawn_local` (not leptos spawn) |
| Play / Next / Prev disabled or no-op | transport `ui_playing`; recover stale Playing + `!play_session_live`; Settling+Play Preempt |
| New clip: Play disabled / silent after N URLs | `retire_clip`: Closed + cancel_all + drop player — engine dispose: `skills/media.md` |
| Hover thumb stuck after new URL | hide on `!ready`; `cancel_hover_thumb` |
| Tab hidden: play freezes or densify spins | visibility → `yield_scrub_work_for_hidden` + 500 ms play tick — `skills/media.md` |
| Play after scrub no-op | `transport` + `ScrubRelease { then_play }`; soft-park on PlayPrime fail; no demo TLS play latch |
| EOF not observed / decoder leak | `VideoRef::ended`; `close()` on unmount / handoff |
| Progress bar stuck on old frame | `player.take()` before zeroing counts; paused rAF `sync_ui` |
| URL open CORS / `/media/` 404 | element needs CORS-ok or same-origin URL; local `/media/` = `public/media/` after `npm run media:fetch` — `architecture.md` Progressive open + `components/README.md` |
| e2e `openUrl` outside viewport / never ready | `DemoPlayer.openUrl` | demo chrome = `btn-load-url` + `prompt`; off-screen `url-input`/`btn-open-url` are not Playwright-clickable. Empty-URL seed → `openEmptyUrl` |
| Gray vs green strips lie / blur | `<TimelineMeta>` + float `paint_strip_canvas` on `f/n·w` — UX: `product.md` |
| Playlist load: gray fills then wipes | `<TimelineMeta>` holds paint until `timeline.ready` (probe open→close) |
| Playlist Exact wrong X / scrub tip jitter | `TimelineRef::strip_*` remap; mirror only while live `playing` — `architecture.md` § Playlist |
| Playlist scrub tip jumps on pointer-up | mirror must not stomp `playhead_ms` from stale `frame` after `scrubbing` clears |
| Playlist multi-open flashes first frames | probe paints on live slot — `visibility:hidden` while `[data-ready=false]` (keep layout; not `display:none`) |
| Playlist scrub→other clip drops Play | latch `scrub_resume` on Down while `playing` (soft-pause clears it before cross-cut) |
| Playlist scrub stuck at cut on drag | never remount `<Scrubber>`; Meta re-leases via `strip_video_key` — `architecture.md` § Playlist |
| Effect side-effect runs on mount | `Effect::watch(deps, handler, false)` — handler skips initial; `Effect::new` runs once after mount |
| Zebra / tip / strip drift | continuous `f/n` (float canvas + tip `calc`); no CSS gradient tiles / integer tip snap |
| Zebra subpixel mush on narrow resize | Host-div `use_resize_observer` + `Show` unmount; gate `FRAME_ZEBRA_MIN_SLOT_PX` (2); never RO the canvas after `set_width` |
| Volume V pop closes in gap above icon | `.wm-volume-slider-v::before` hit-bridge = `--wm-volume-v-gap` (keeps `:hover` while crossing) |
| Volume V stuck open until click | drag → `is-sliding` only (not `volume_open`); blur on pointerup; mouseleave clears open on hover devices |
| Rate pop alignment | `.wm-rate-pop` → `right: 4px` on `.wm-controls` (`.wm-rate` not positioned); track `min(17.5rem, 100vw − 1rem)` |
| Demo scrubber touches `pipeline` | compose `<Scrubber>` / `<TimelineMeta>` / `<HoverThumb>` — `product.md` § Element-layer helpers |
| Hover jank on Play / lag until paint | `<HoverThumb>` trailing 150 ms; `style:transform` from signal (not `set_property`); engine: `skills/media.md` |
| Thumbs / progressive on phone waste CPU | host `(hover: none)` → `ThumbPolicy::Off` + no `<HoverThumb>` (not `maxTouchPoints`) — `product.md` |
| Thumbs On again after every open | `HostCtx.thumb_policy` + `run_open` re-apply (mirror scrub cache) |
| `RefCell already borrowed` (tachys / Loop) | no borrow across await; scrub → `scrub_pointer_*` sync (Move throttled in `<Video>`); no demo TLS |
| Start stutter / Play lag after scrub | `prepare_play_from_scrub` + clock after prime; no `seek_frame` from `VideoRef::play`; engine: `skills/media.md` scrub→play |
| Scrub bar lags / pointer jank / hold jumps | sync playhead on move; no sync Exact present on move (ScrubDrag owns canvas); dwell ~100 ms on blit miss; paused rAF `sync_ui` ≤10 Hz while `Scrubbing`; engine: `skills/media.md` soft-drag |
| Touch playhead jitter on long clips | `<Scrubber>` touch sticky: `|Δx|≥4px` + `|Δframe|≥ceil(n/w)`; mouse/pen 1:1 — `scrub_touch_move_accepts` |
| EOF: tip/stats on end−1/−2, canvas last frame | `stop_playback` snap when `leaving_play \|\| ended` — `Ended` clears `playing` before snap |
| Play on last frame → ½ s audio blip / instant End | `run_play_prime` snap playhead→0 before clock; loop on/off same. `mediabunny: media-player play` endTimestamp→first |
| Ops dropped while busy | OpQueue coalesce — not `if busy { return }` (`host_op_coalesce`) |

## Quick reference

| Need | Use |
| ---- | --- |
| Local state | `RwSignal` / `Memo` |
| Browser async | `LocalResource` / `wasm_bindgen_futures::spawn_local` |
| DOM / rAF / resize / events | **`leptos-use`** — not raw `Closure` |
| Effect after change only | `Effect::watch(deps, handler, false)` — not on mount |
| Canvas / Performance / File | raw `web-sys` OK |
| CSS vars / inline geometry | declarative `style=` / `class=` from signals — **not** `el.style().set_property` |

Tailwind: only literal class strings in `.rs`/`.css`; prefer canonical utilities over `[…]` (see `architecture.md` § Stack → Styling); computed geometry → `style=` from a signal (measure in Effect/rAF → write signal → view binds).

## Host gotchas

1. **OpQueue** — serialize open/seek/play-prime (CallSerializer + asyncId). `busy` = draining, not “drop input”.
2. **No `RefCell` across `.await`** — clone `Rc<VideoPlayer>`, await, put back. Engine re-entry: `skills/media.md` RefCell.
3. **`playing` follows transport** — `TransportState::ui_playing(session_live)` (`Playing`, or priming/resume-settle after `begin_play_session`). Never `playing.set(engine.playing)` in `sync_ui`. Seek/Step/Play/Pause/scrub → `dispatch_transport` / `note_transport`.
4. **Transport** — pointers → `VideoRef::scrub_pointer_*`; Seek/Step → `SeekRequested` / `StepRequested`. FSM owns resume (`ScrubRelease { then_play }`). Soft-hold via `pause_for_scrub`. PlayPrime fail → soft-park (no bump) so retry works.
5. **Playhead** — sync on pointerdown/move before `scrub_pointer_*`. `sync_ui` updates playhead only while playing or on new clip. Leaving play: snap playhead to engine. Hold ←/→: aim `engine±1`, not `playhead++`.
6. **Clock / play session** — `VideoPlayer::media_time()` only; engine owns prefetch, underrun, loop seam, silent-clock arm (`play_raf_tick` / `begin_play_session`). Host: OpQueue prime + rAF tick; visibility → scrub yield + hidden play interval (not clock hold).
7. **rAF** — `use_raf_fn` resume after prime; throttle `sync_ui`/fps ~10 Hz; call `play_raf_tick` (not host underrun/loop).
8. **Open / clip switch** — file input uses wasm spawn; `retire_clip` before zeroing UI; remote URL → host CORS / same-origin then UrlSource/blob (demo `/media/` static files locally; not the element crate).
9. **`ended` / `close`** — Ended only from engine EOF (loop off); clear on open/seek/play/scrub; `close` = cancel_all + drop player.
10. **Cancelled** — soft (no error banner). Clear sticky `error` on Ok settle.
