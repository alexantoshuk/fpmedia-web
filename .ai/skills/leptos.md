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
| Playlist thumbs only on live clip | max **2** slot-bound `ThumbSink`s (host policy on both); idle densify off; hover rebinds idle for off-slot clips — not N / not app-wide shared |
| Scrub 3rd clip shows 1st media | `swap_live_idle`: `idle_clip` = former live only; neighbor via `preload_idle` — never label aspirational clip |
| Playlist Exact wrong X / scrub tip jitter | `TimelineRef::strip_*` remap (global frames); mirror live→pos when not `scrubbing` (paused step OK; scrub owns aim) — `architecture.md` § Playlist |
| Playlist scrub tip jumps on pointer-up | mirror: if slot `playhead ≠ frame` map from playhead (settle lag); Up clears `scrubbing` only after aim `playhead`/`pos` set. **no equivalent** |
| Playlist zebra gate / stripe size wrong | axis = Σ frames (not ms); same `frame_zebra_step` as single-clip |
| Playlist multi-open flashes first frames | probe paints on live slot — `visibility:hidden` while `[data-ready=false]` (keep layout; not `display:none`) |
| Playlist scrub→other clip drops Play | latch `scrub_resume` on Down while `playing` (soft-pause clears it before cross-cut) |
| Playlist seam densify lag (cut / wrap / range host) | `swap_live_idle` + policy On (no densify arm); seam `prepare_seam_at` → densify-free seek — `skills/media.md` |
| Playlist cut@0 double warm / Origin vs Mid fork | `PromoteLead` in `slots` — one prepare/kick table; no post-swap `warm_play_origin`. Port map: `architecture.md` § LoopSeam vs host promote. **no equivalent** |
| Playlist near-wall warm ad-hoc if soup | `seam_warm_schedule` → target; kick when `SeamPhase::Idle` + owner ≠ Engine. **no equivalent** |
| Playlist Next/Prev stuck in clip at EOF/0 | `plan_step` + live `frame`; mirror live→pos when not scrubbing |
| Playlist loop wrap hitch (last→0) | `idle_neighbor(…, looping)` → idle preload **0**; `set_loop(true)` re-preloads. **no equivalent** |
| Playlist LoopZone mid-clip past out / same-clip HF flash | `LoopOwner::Engine` → sync + engine LoopSeam; `Host` → `wrap_to_loop_in` under `SeamPhase`. **no equivalent** |
| Playlist Host: stop at clip join / Play → playlist@0 | clear `ended` only after `try_begin_seam`; Play at zone/clip end → wrap or promote+play (Host `looping=false` must not hit PlayPrime snap→0). Wall = `g >= zone_out-1` **on live clip only**. Never `advance` early-return on sticky `playing` after Effect cleared `ended`. **no equivalent** |
| Playlist Host LoopZone: dual tip / flaky stop after wall | mirror skip while `seam_busy` (wrap owns aim); re-assert `zone_in` before `end_seam` — else mirror fights snap → wall re-wrap aborts PlayPrime. Trace: `LOOP_SEAM_TRACE`. **no equivalent** |
| Playlist Host small LoopZone: stop right after wrap PlayPrime ok | near-wall `kick_prepare` warm overlaps wrap; late `seek_frame_with` clears play session. `prepare_playlist_seam_at` `begin_op` before warm. Log: `prepare_playlist_seam cancelled`. **no equivalent** |
| Playlist Host small LoopZone: pause icon + frozen progress after wrap | never `kick_warm` / Mid prepare on Playing live (`on_idle` only) — post-wrap still inside 1s prewarm → live prepare kills session; facade `playing` stays. **no equivalent** |
| Playlist Host: video past loop / scrubber frozen during wrap (mobile) | `stop_live_for_host_seam` before idle prepare — mirror gated + live still Playing → canvas overruns while tip stuck; facade stays playing. **no equivalent** |
| Playlist Next/goto/scrub clamped to LoopZone | Next/Prev → `plan_step` (ignore zone); goto on ref; paused scrub ignore zone; scrub-while-play resume = Play (outside → `loop_in`). `glossary.md` § Goto vs Seek. **no equivalent** |
| Playlist Host LoopZone: scrub-left before `in` / Play outside → cancel or double wrap | Up → `plan_scrub_up`; Play + wall Effect → `plan_host_outside` + `start_host_wrap`. Soft/hard after wrap → `plan_wrap_done` / `run_wrap_done`. `architecture.md` § Playlist host reduce. **no equivalent** |
| Playlist clip join hitch + Covered/Exact storm | `stop_playback` → `pause_hard_no_densify` (not `pause` densify). Pause button still `pause_warm_idle`. **no equivalent** |
| LoopZone 2-clip: pause@out → 2nd@0 → green → in | `try_begin_seam` before spawn; paint-before-swap `prepare_seam_at(loop_in)` (not pause+cold `seek_to_pos`); Host wall → wrap. **no equivalent** |
| Playlist seam volume jump | `VideoPlaylistRef::volume`/`muted` + `apply_gain_to_slots`; demo binds playlist gain, not live slot |
| Playlist Play↔Pause icon flash at clip join / wrap | Facade `VideoPlaylistRef::playing` (intent); demo `media_playing` — not live slot. Slot EOF/`pause_hard` clears slot only. Clear facade on pause / terminal end / seam error. **no equivalent** |
| Playlist goto/Next/Prev: Pause icon while audio still plays | edge Next/Prev → `GotoPos` → `goto_*` → `stop_play_for_goto` before seek; mid → clear intent + live step. Internal `seek_*` does not stop intent. **no equivalent** |
| Playlist Host wrap: Pause frozen / Play icon after failed prepare | `plan_wrap_done` → soft `ResumeLiveKeepPlay` / hard clear Play+error (`run_wrap_done`); live already `stop_live_for_host_seam`. **no equivalent** |
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
