# Leptos 0.8.x (`fpmedia-web-components` / `demo-player`)

> Read before touching components, resources, or routing. Pins **Leptos 0.8.x**.
> Engine decode/scrub hangs → `skills/media.md`. Stack/CSR/leptos-use → `architecture.md`.

## Triage

| Symptom | Look first |
| ------- | ---------- |
| Android Browse / file picker dead | `.wm-file input` not `display:none` — opacity overlay |
| Open stuck after `open_blob start` | `wasm_bindgen_futures::spawn_local` (not leptos spawn) |
| Play / Next / Prev disabled or no-op | transport `ui_playing`; recover stale Playing + `!play_session_live`; Settling+Play Preempt |
| Video scrub-while-play flips Pause→Play (incl. Up flash) | `ui_playing` = intent through `Scrubbing`/`Settling` `{ resume: true }` (not `play_session_live`). Stale recover only `Playing && !live`. `product.md` § Play chrome. **no equivalent** |
| New clip: Play disabled / silent after N URLs | `retire_clip`: Closed + cancel_all + drop player — engine dispose: `skills/media.md` |
| Hover thumb stuck after new URL | hide on `!ready`; `cancel_hover_thumb` |
| Tab hidden: play freezes or densify spins | visibility → `yield_scrub_work_for_hidden` + 500 ms play tick — `skills/media.md` |
| Play after scrub no-op | `transport` + `ScrubRelease { then_play }`; soft-park on PlayPrime fail; no demo TLS play latch |
| EOF not observed / decoder leak | `VideoRef::ended`; `close()` on unmount / handoff |
| Progress bar stuck on old frame | `player.take()` before zeroing counts; paused rAF `sync_ui` |
| URL open CORS / `/media/` 404 | element needs CORS-ok or same-origin URL; local `/media/` = `public/media/` after `npm run media:fetch` — `architecture.md` Progressive open + `components/README.md` |
| e2e `openUrl` outside viewport / never ready | `DemoPlayer.openUrl` | demo chrome = `btn-load-url` + `prompt`; off-screen `url-input`/`btn-open-url` are not Playwright-clickable. Empty-URL seed → `openEmptyUrl` |
| Gray vs green strips lie / blur | `<TimelineMeta>` + float `paint_strip_canvas` on `f/n·w` — UX: `product.md` |
| Playlist load: gray fills then wipes / first frames flash | `<TimelineMeta>` holds paint until `timeline.ready` (probe open→close); probe paints on live slot — `visibility:hidden` while `[data-ready=false]` (not `display:none`) |
| Playlist thumbs empty far clips / hover or Play hitch / empty tip | VT `VtThumbCache` + `ThumbProducer` / `kick_vt_thumb_fill` (producer may `open_mp4` any clip, Pause only; not idle). Hover must **not** `ensure_clip_on_idle` / `warm_idle_for_clip`; `<HoverThumb>` paint-first from VT, miss keeps prior, re-paint on `thumb_residency_epoch`. **no equivalent** |
| Playlist scrub 3rd clip shows 1st media / progress freezes after join | `swap_live_idle`: `idle_clip` = former live only (neighbor via `preload_idle`, never aspirational); `promote_idle_at` sets facade `pos`/`playhead`; `end_seam` before `preload_idle` (mirror was gated). **no equivalent** |
| Playlist Exact wrong X / scrub tip jitter / tip jumps on pointer-up | `TimelineRef::strip_*` remap (global frames); mirror live→pos only when not `scrubbing`: slot `playhead ≠ frame` → use playhead; skip while `pos.clip ≠ live` (CrossClipSeek); Up clears `scrubbing` after aim; `seek_to_pos` sets live playhead before preload await — `architecture.md` § Playlist composition. **no equivalent** |
| Playlist paused scrub Up: gray + empty Exact / cut Up cold Exact | `seek_to_pos(!then_play)`: idle `prepare_seam_at` → swap → PresentHit or `seek_frame` (densify); never `arm_seam_hold` on pause (Play keeps SeamHold). Cut join: `kick_cut_join_exact_warm` / `cut_join_local` (Pause one-shot On→Exact@join→park; not Move thrash). PresentHit: `clear_densify_hold` before arm. **no equivalent** |
| Playlist Exact drains on promote / dead after neighbor / reverse cut (2→1) / 1\|2 lag | Demote `park_scrub_cache` (not Off wipe); live On + densify on promote/settle. `ensure_clip_on_idle` / `preload_idle`: after `wait_ready` park only if still idle, if `live_clip==opened` → On; stale ensure must not park live; cancel after BeginExact → EndExact. Idle = `PlaylistModel::idle_neighbor_near_cut` / `kick_ensure_idle` (nearest cut, mid hysteresis). Cross-cut scrub: `pause_hard_no_densify` then `pause()` (not bare `PausePressed`). `architecture.md` § Virtual Timeline. **no equivalent** |
| Playlist Covered / Fetch strip dies after leave clip | `mirror_covered_from_slots` / `clear_vt_covered` / `mirror_fetch_from_slots` / `strip_fetch_marks` — both = VT ledger; park/rebind must not clear. Covered wipe: user Scrub Off → `VideoPlaylistRef::set_scrub_cache_policy`; Fetch wipe: close·open only. **no equivalent** |
| Playlist sticky `operation cancelled` / scrub→other clip drops Play | `seek_pos` / `seek_frame`: ignore `is_soft_seam_cancel`. Latch `scrub_resume` on Down while `playing` (soft-pause clears slot before cross-cut) |
| Playlist rapid taps during Play: `media source: no player` | `plan_idle_prepare` / `retire_clip` | overlapping seeks `close()` idle, then a stale seek prepares it. `ready=false` before `player.take` (pause/drop re-enters). Live+player → stay on live; idle+player → seam; missing player → `Cancelled`. **no equivalent** |
| Playlist zebra gate / stripe size wrong | axis = Σ frames (not ms); same `frame_zebra_step` as single-clip |
| Playlist seam / join / wrap hitch (picture+progress freeze, densify lag, double warm) | Play path: idle `arm_seam_hold` before EOF → `swap_live_idle` (policy On, no densify arm) → PlayPrime **release** (not SW stash + cold prime); paint `prepare_seam_at` densify-free. One table: `PromoteLead` / `seam_warm_schedule` → `kick_arm_seam_hold` (kick when `SeamPhase::Idle` + owner ≠ Engine); no post-swap `warm_play_origin`. Join: `stop_playback` → `pause_hard_no_densify`. Port map: `architecture.md` § LoopSeam vs host promote; engine `skills/media.md`. **no equivalent** |
| Playlist loop wrap hitch (last→0) / LoopZone mid-clip | `idle_neighbor(…, looping)` → idle preload **0**; `set_loop(true)` re-preloads. `LoopOwner::Engine` → sync + engine LoopSeam; `Host` → `wrap_to_loop_in` under `SeamPhase`. **no equivalent** |
| Playlist Next/Prev stuck / clamped to LoopZone | `plan_step` + live `frame` (ignores zone); goto on ref; paused scrub ignores zone; scrub-while-play resume = Play (outside → `loop_in`). `glossary.md` § Goto vs Seek. **no equivalent** |
| Playlist Host: stop at clip join / Play → playlist@0 / first mid-zone join | clear `ended` only after `try_begin_seam`; EOF → `plan_advance_ended`; Play@clip-last → `plan_play_at_clip_last`. Wall = `g >= zone_out-1` **on live only**. Promote/`wrap` sets `seam_hold_restart_on_release` when Host+!zone_full; PlayPrime → `release_playlist_seam_hold(true)` (engine: `skills/media.md`; trace `LOOP_SEAM_TRACE`). **no equivalent** |
| Playlist Host LoopZone: dual tip / frozen progress / stop after wrap / mobile overrun | mirror skip while `seam_busy`; re-assert `zone_in` before `end_seam`; `stop_live_for_host_seam` before idle prepare; never `kick_warm` / Mid prepare on Playing live (`on_idle` only); `prepare_playlist_seam_at` `begin_op` before warm (log `prepare_playlist_seam cancelled`). **no equivalent** |
| Playlist Host LoopZone: scrub-left before `in` / Play outside / 2-clip pause@out / failed prepare | VT `plan_scrub_up` / `plan_host_outside` (`loop_zone`) → `start_host_wrap`; `try_begin_seam` before spawn; paint-before-swap `prepare_seam_at(loop_in)`. Soft/hard → `plan_wrap_done` (`ResumeLiveKeepPlay` / clear Play+error via `run_wrap_done`). `architecture.md` § VT / Playlist host plans. **no equivalent** |
| Playlist volume / rate jump / Play↔Pause icon flash / Pause icon after goto | Facade `VideoPlaylistRef` `volume`/`muted`/`playback_rate`/`playing` (intent) + `apply_gain_to_slots`; demo binds facade (`media_playing`), not live slot. Slot EOF/`pause_hard` clears slot only; facade clears on pause / terminal end / seam error. Edge Next/Prev → `GotoPos` → `goto_*` → `stop_play_for_goto`; internal `seek_*` does not stop intent. **no equivalent** |
| Playlist scrub stuck at cut on drag | never remount `<Scrubber>`; Meta re-leases via `strip_video_key` — `architecture.md` § Playlist composition |
| Playlist click-hold on another clip keeps the old picture | `kick_ensure_idle` → `scrub_hold_from_idle` (promote + `scrub_pointer_down`, same as a later Move). **no equivalent** |
| First playlist goto to an unvisited clip updates the readout and leaves the old picture | `seek_paint_clip` while `seek_to_pos` paints idle: join warm must not `prepare_seam_at` / park (both `begin_op`). In-flight open of that clip is waited, not closed. **no equivalent** |
| Effect side-effect runs on mount | `Effect::watch(deps, handler, false)` — handler skips initial; `Effect::new` runs once after mount |
| Zebra / tip / strip drift | continuous `f/n` (float canvas + tip `calc`); no CSS gradient tiles / integer tip snap |
| Zebra subpixel mush on narrow resize | Host-div `use_resize_observer` + `Show` unmount; gate `FRAME_ZEBRA_MIN_SLOT_PX` (2); never RO the canvas after `set_width` |
| Volume V pop closes in gap above icon | `.wm-volume-slider-v::before` hit-bridge = `--wm-pop-gap` (keeps `:hover` while crossing) |
| Volume V stuck open until click | drag → `is-sliding` only (not `volume_open`); blur on pointerup; mouseleave clears open on hover devices |
| Rate / volume / settings / position pop height mismatch | shared `--wm-pop-gap`; rate anchors on `.wm-rate` (`relative`), not `.wm-controls` |
| Demo scrubber touches `pipeline` | compose `<Scrubber>` / `<TimelineMeta>` / `<HoverThumb>` — `product.md` § Element-layer helpers |
| Hover jank on Play / lag until paint | `<HoverThumb>` trailing 150 ms; `style:transform` from signal (not `set_property`); engine: `skills/media.md` |
| Thumbs / progressive on phone waste CPU | host `(hover: none)` → `ThumbPolicy::Off` + no `<HoverThumb>` (not `maxTouchPoints`) — `product.md` |
| Thumbs On again after every open | `HostCtx.thumb_policy` + `run_open` re-apply (mirror scrub cache) |
| `RefCell already borrowed` (tachys / Loop) | no borrow across await; scrub → `scrub_pointer_*` sync (Move throttled in `<Video>`); no demo TLS |
| Start stutter / Play lag after scrub | `prepare_play_from_scrub` + clock after prime; no `seek_frame` from `VideoRef::play`; engine: `skills/media.md` scrub→play |
| Scrub bar lags / pointer jank / hold jumps | sync playhead on move; no sync Exact present on move (ScrubDrag owns canvas); dwell ~100 ms; paused rAF `sync_ui` ≤10 Hz while `Scrubbing`; engine: `skills/media.md` soft-drag |
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
2. **No `RefCell` across `.await`** — clone `VideoPlayer`, await, put back. Engine re-entry: `skills/media.md` RefCell.
3. **`playing` follows transport** — `TransportState::ui_playing(session_live)` = play intent (`Playing`, resume `Scrubbing`/`Settling`; `PlayPriming` needs live session). Never `playing.set(engine.playing)` in `sync_ui`. Seek/Step/Play/Pause/scrub → `dispatch_transport` / `note_transport`.
4. **Transport** — pointers → `VideoRef::scrub_pointer_*`; Seek/Step → `SeekRequested` / `StepRequested`. FSM owns resume (`ScrubRelease { then_play }`). Soft-hold via `pause_for_scrub`. PlayPrime fail → soft-park (no bump) so retry works.
5. **Playhead** — sync on pointerdown/move before `scrub_pointer_*`. `sync_ui` updates playhead only while playing or on new clip. Leaving play: snap playhead to engine. Hold ←/→: aim `engine±1`, not `playhead++`.
6. **Clock / play session** — `VideoPlayer::media_time()` only; engine owns prefetch, underrun, loop seam, silent-clock arm (`play_raf_tick` / `begin_play_session`). Host: OpQueue prime + `use_raf_fn` tick (resume after prime; `sync_ui`/fps ~10 Hz; one `on_play_tick` shared with the hidden-tab interval — not host underrun/loop); visibility → scrub yield (not clock hold).
7. **Open / clip switch** — file input uses wasm spawn; `retire_clip` before zeroing UI; remote URL → host CORS / same-origin then UrlSource/blob (demo `/media/` static files locally; not the element crate).
8. **`ended` / `close`** — Ended only from engine EOF (loop off); clear on open/seek/play/scrub; `close` = cancel_all + drop player. **Cancelled** is soft (no error banner); clear sticky `error` on Ok settle.
9. **Host handles, not callback bags** — `VideoRef` → `VideoHost`, `VideoPlaylistRef` → `PlaylistHost` (+ `mounted` latch); `live_video()` / `idle_video()` (+ `_tracked`) pick the slot — no inline `live_is_a` branches, no `Rc<dyn Fn>` op structs. `architecture.md` § Ownership & types.
