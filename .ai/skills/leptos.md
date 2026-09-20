# Leptos 0.8.x skill notes

> Read before touching any component, resource, or routing code.
> Project pins **Leptos 0.8.x**. 0.6/0.7 examples **won't compile**.

---

## Triage

| Symptom | Look first |
| ------- | ---------- |
| Browse / file picker does nothing on Android Chrome | `.wm-file input` must not be `display:none` — opacity overlay (see gotcha) |
| Open never finishes / console stops at `open_blob start` | File-input `wasm_bindgen_futures::spawn_local` (not leptos spawn) |
| Play / Next / Prev disabled or silent no-op | UI-owned `playing`; recover when session is `None` |
| Play after scrub does nothing (esp. after EOF) | Defer UI `playing` until after prime+clock; snap clock; **scrub drag throttle `trailing(false)`**; Play→scrub→release: `defer_scrub_finish` applies settle **then** `play()` (immediate `play()` loses to `enqueue_scrub` dropping PlayPrime) |
| EOF with `loop` off — host never notified | `VideoRef::ended`; clear on open/close/seek/play |
| Decoder/cache leak after leaving a clip | `VideoRef::close()` (also on unmount) |
| URL open hangs / CORS / slow full download | `VideoRef::open_url` → `UrlSource` (Range) or blob fallback |
| `RefCell already borrowed` in Loop / sync_ui / tachys | No borrow across `.await`; defer scrub work off `on:pointer*`; engine: `skills/media.md` RefCell / WASM re-entry |
| Start stutter / low measured fps after Play | Set wall-clock origins **after** `prime_playback` |
| Play lag right after scrub (long GOP) | Do not `seek_frame` from `VideoRef::play` — waits behind settle; densify only if no pending PlayPrime |
| Pause→Seek same / Prev→Next ≠ pause picture | Snap `playhead` to engine on leaving play (`stop_playback`) |
| Scrub pointer jank / re-entrant park | Don’t `pause` every `pointermove` while paused; async scrub |
| Fullscreen scrub / Next·Prev flood stalls UI | Scrub drag throttle (~50 ms); Next/Prev: hold-safe playhead + OpQueue coalesce |
| Next/Prev: lag then several frames jump | Hold key-repeat: hold-safe playhead (no Step→Seek); Step always VF; drain rAF |
| Ops dropped / ignored while “busy” | OpQueue serialize — not `if busy { return }` |

Decode / densify / WebCodecs hangs → `skills/media.md` triage.

## Quick reference

| Need | Use |
| ---- | --- |
| Local mutable state | `RwSignal::new(val)` |
| Derived value | `Memo::new(\|_\| expr)` |
| Read / write signal | `sig.get()` / `sig.set(val)` |
| Browser-only async | `LocalResource::new(\|\| async { … })` |
| Shared state | `provide_context` / `use_context::<T>()` |
| Navigation | `use_navigate()` |
| DOM node | `NodeRef::<html::Canvas>` (etc.) |
| Resize / observe / DOM events / rAF helpers | **`leptos-use`** (`use_resize_observer`, `use_event_listener`, `use_raf_fn`, …) — not raw `web-sys` + `Closure` |

Prefer `leptos-use` whenever it covers the Web API. Raw `web-sys` in `video/` for media plumbing (canvas, `Performance`); in `app/` only for demo File / input element types.

---

## Rendering model (CSR)

The server serves a static `index.html` shell + WASM; `App` mounts via `mount_to_body`. There is **no** SSR HTML render. `HydrationScripts` in `shell()` is cargo-leptos' WASM loader. `frontend::hydrate()` calls `mount_to_body(App)`.

### Mount stability

Long-lived chrome (`<Video/>`, providers) must survive route changes — mount **outside** matched parametric route views, or keep a single-page demo without remounting children via `{move || match … => view! { <Child/> }}`.

### Resources in CSR

`resource.get()` in a reactive view closure is fine; show loading UI while `None`. Prefer `LocalResource` for File / fetch / open (no server fn).

---

## Tailwind

Only classes that appear **literally** in `.rs` / `.css` are emitted. Do not build class names with `format!()` for Tailwind utilities; use `style=` for computed geometry.

---

## Video element gotchas (`video` crate)

1. Open / seek / play-prime are **async** and share `&mut`-equivalent `WebPlayer` state — serialize them on one local **op queue** (`OpQueue`, mediabunny `CallSerializer` + demo `asyncId`). Do not gate with ad-hoc `if busy { return }` (that drops user input). `busy` means the queue is draining.
2. Do not call `pause` / `is_playing` on the Rust player while another async method is awaiting (aliasing). Mirror play state in a Leptos signal for the UI. **Never hold `RefCell`/`borrow_mut` across `.await`** — clone the `Rc<WebPlayer>`, await, then put it back (otherwise Loop/sync_ui panics: `RefCell already borrowed`).
3. Canvas: pass `HtmlCanvasElement` from `NodeRef` after mount; open player once canvas exists.
4. Status **fps** is measured display rate (presents / wall time over ~1s), not container/`1/duration` — that stays near source fps even when decode lags. Source fps / codec / size come from `WebPlayer::metadata()` (shown in demo `.wm-meta`).
5. **Playback pacing** — prefer [`WebPlayer::media_time`] (engine `MediaClock`: wall until audio, then `AudioContext`). Not per-frame `setTimeout`; do not open `AudioContext` in the host. **`resume_audio_context` on Play before `prime_playback`** — priming can take hundreds of ms; late `resume()` leaves the context suspended → silent audio. Then arm the clock after prime.
6. **Prefetch + underrun hold** — prime on Play; background pump fills until `PRESENTATION_QUEUE_CAP` (ahead=4; step behind ring is separate). Overdue frames already in the queue are skipped in `present_due`. Empty queue mid-play → freeze media clock (`holding_for_buffer`) until prefetch has a frame, then `start_media_clock` again (not `skip_behind`). Prefetch keeps running while holding.
7. **Play UI cost** — throttle `sync_ui` / fps (~100 ms). No `busy` toggles during the rAF loop.
8. **Viewport layout** — `.wm-viewport` is a fixed media stage; canvas uses contain (`max-width/max-height: 100%` + source `aspect-ratio`). Call `WebPlayer::relayout()` after open; `use_resize_observer` on the viewport NodeRef also calls `relayout` (try_borrow — skip if player RefCell is busy).
9. **`leptos-use` playback** — present loop = `use_raf_fn_with_options(..., immediate: false)`; `resume()` only **after** `prime_playback` + media-clock origins. Pause/EOF clears a shared `playing` signal; an `Effect` calls `pause()` on the Pausable. Async `loop_to_start` sets a session `restarting` flag so rAF skips until origins reset. Prefetch stays a separate `spawn_local` with a thin await-rAF helper — `use_raf_fn` is callback-oriented and cannot `.await` WebCodecs.
10. **Loop clock** — reset origins only **after** `loop_to_start`. Prefer `prepare_loop_if_needed` once EOF is queued (same decoder → `loop_prime`) so the seam is a swap; cold restart only if prepare missed. Engine near-EOF presentation cap scales with rate (see `skills/media.md` loop seam gotcha) so review rates finish prime before drain.
11. **`VideoRef` ops** — non-`Send` callbacks live in `StoredValue::new_local`; do not put `Rc<dyn Fn>` in a default `RwSignal` / `StoredValue` (needs `Send + Sync`).
12. **Cancel / epochs** — treat `MediaError::Cancelled` as soft (no error banner). One host `async_id` (`OpQueue::bump`) invalidates open / play-prime / prefetch (mediabunny player `asyncId`). Open and pause bump; seek/play enqueue without bumping so they can follow an in-flight open. `WebPlayer::cancel_ops` still aborts decode. Prefetch rAF: use a `Closure` (no bare Promise `resolve`).
13. **Op queue policy** — Open: bump + clear pending + enqueue. Seek/step: pause transport, coalesce pending seek/step/play (keep open), enqueue. Play-prime: replace pending PlayPrime only. Drain is a single `wasm_bindgen_futures::spawn_local` loop (file-input is outside a Leptos owner).
14. **Open from file input** must use `wasm_bindgen_futures::spawn_local`, not `leptos::task::spawn_local` — the picker runs outside a Leptos reactive owner; leptos spawn can leave the future unscheduled (UI stuck after `open_blob start`).
14b. **Android Chrome Browse** — do not `display:none` the demo `.wm-file input[type=file]`; Android ignores label activation on that. Keep the input in layout with `opacity: 0` covering the label (`style/input.css`).
15. **Transport `playing` is UI-owned** — never `playing.set(engine.playing)` in `sync_ui`. After Pause, that clobber left `playing=true` with no session → Play/Next/Prev all `disabled` / silent no-op. Recover stale `playing` when session is `None`.
16. **Quick Pause→Play** — cancelled prefetch may still hold the decoder slot; engine `decode_frame` must wait/yield (not fail `decoder busy`). Stale play tasks must not stop the new session unless `async_id` still matches.
16b. **Play right after scrub** — `VideoRef::play` must **not** call `seek_frame` when `frame != playhead`. Settle is already cascading to playhead; a Play-time seek cancels it and restarts the GOP (seconds on Wing It). Queue PlayPrime behind settle; `run_play_prime` seeks only if still mismatched. After settle, arm densify only when `!has_pending_play()` (`arm_scrub_densify_after_exact`) so Play keeps a warm main decoder. `prime_playback` lifts contiguous dense frames into the presentation queue (move, not clone). **Do not set UI `playing` until prime + `start_media_clock` + session exist** — earlier rAF + scrub-stale `media_time` (still at EOF after Ended) → `DuePresent::Ended` → `stop_playback` bumps `async_id` and kills PlayPrime (silent no-op). Engine snaps `MediaClock` on successful scrub/seek/step. rAF present path requires a session. **Scrub drag throttle must use `trailing(false)`** — leptos-use trailing edge can fire after pointerup settle + PlayPrime and enqueue a phantom drag that clears `playing` (EOF→scrub→Play smoke).
17. **Scrub pointer events** — do not call `WebPlayer::pause` / heavy park on every `pointermove` while already paused (re-enters scrub pipeline). Defer `scrub_frame` / `scrub_settle` with `wasm_bindgen_futures::spawn_local` so work runs after tachys releases the event-callback `RefCell` — sync engine work inside `on:pointer*` can nest into the same handler → panic `RefCell already borrowed` in `tachys` `event.rs`. Drag → `scrub_frame` (densify on); pointer up → `scrub_settle` (exact-first). **Throttle scrub drag only** (`use_throttle_fn_with_arg_and_options`, ~50 ms, `leading(true).trailing(false)` — trailing after pointerup races PlayPrime); settle immediate (exacts live playhead). Next/Prev are **not** throttled — OpQueue coalesce + hold-safe playhead aim. **Step drain** — after each `HostOp::Step`, `wait_animation_frame` so cached hits do not paint several frames in one task. Async scrub (`HostOp::Scrub`): cancel in-flight + latest wins, no `busy`; key preview then exact. Sync Prev/Next/Seek latch `busy` and wait their turn (preempt scrub only). Arrow keys: enqueue while busy (OpQueue coalesces); do not `if busy { return }` (that drops repeats then bursts).
18. **UI playhead** — `VideoRef::playhead` is the potential frame (scrubber / Prev / Next / Seek / Play / Pause). Scrub updates it immediately and does not latch `busy`. Sync ops promote playhead and drive the engine from it (adjacent step uses `previous_frame`/`next_frame` when `frame` is next to playhead; otherwise `seek_frame`). **Hold ←/→** — do **not** `playhead += 1` on every key-repeat; that races ahead of `frame` and promotes Step→Seek (multi-frame jump). Aim at most `engine±1`, coalesce Step; after Step snap playhead to committed engine. `sync_ui` copies engine → playhead only while playing or when `frame_count` changes (new clip) — never clobber a paused scrub target. **Leaving play** (`stop_playback`): after `sync_ui`, snap `playhead = frame` — during play UI lags ~100ms, and `sync_ui` while already `!playing` would leave playhead stale → Pause→Seek/Prev/Next jump off the canvas frame. `VideoRef::pause` must not seek to that stale playhead when leaving play; only promote `playhead→seek` when already paused (scrub).
19. **`ended` + `close`** — `DuePresent::Ended` (loop off) sets `VideoRef::ended` then `stop_playback`; pause alone must **not** set `ended`. Clear `ended` on open / close / play / seek / scrub / step. `close()` = `OpQueue::cancel_all` + drop `WebPlayer` (decoder/caches) while keeping the canvas mounted; call from `on_cleanup` too. Hosts (playlist handoff) watch `ended` and call `close` on the retiring slot.
20. **`open_url`** — `https:`/`http:` → `UrlSource` (HTTP Range; mediabunny: `src/source.ts` `UrlSource` / `_runWorker`) when server returns `206`+`Content-Range`; else full fetch → `BlobSource`. Same-doc `blob:` → fetch→Blob. Needs CORS. Supersede in-flight via `OpQueue::cancel_all` + `current_id` before `enqueue_open`. Local files: file picker → `open_blob`.

Decode / scrub / densify engine traps: `skills/media.md`.
