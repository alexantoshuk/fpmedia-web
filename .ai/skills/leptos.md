# Leptos 0.8.x (`media_components` / `app`)

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
| Tab hidden: audio dies / video races | `visibilitychange` → HoldHidden / resume — `skills/media.md` |
| Play after scrub no-op | `transport` + `ScrubRelease { then_play }`; soft-park on PlayPrime fail; no demo TLS play latch |
| EOF not observed / decoder leak | `VideoRef::ended`; `close()` on unmount / handoff |
| Progress bar stuck on old frame | `player.take()` before zeroing counts; paused rAF `sync_ui` |
| URL open CORS / `/proxy` 403 / CL mismatch | same-origin proxy, loopback only — `architecture.md` Progressive open |
| Gray vs green strips lie / blur | 1 px canvas `paint_strip_canvas_1px`; greens honest snap (no soft-merge / min-px) — UX: `product.md` |
| Hover jank on Play / lag until paint | `HoverThumbPreview` (interim; agreed `<HoverThumb>` → `product.md`) trailing 150 ms; sync transform first; engine: `skills/media.md` |
| `RefCell already borrowed` (tachys / Loop) | no borrow across await; scrub → `scrub_pointer_*` sync (Move throttled in `<Video>`); no demo TLS |
| Start stutter / Play lag after scrub | clock origins after prime; no `seek_frame` from `VideoRef::play` |
| Scrub bar lags / pointer jank / hold jumps | sync playhead on move; `<Video>` Move throttle 50 ms; paused rAF `sync_ui` ≤10 Hz (150 ms while `Scrubbing`) |
| Ops dropped while busy | OpQueue coalesce — not `if busy { return }` (`host_op_coalesce`) |

## Quick reference

| Need | Use |
| ---- | --- |
| Local state | `RwSignal` / `Memo` |
| Browser async | `LocalResource` / `wasm_bindgen_futures::spawn_local` |
| DOM / rAF / resize / events | **`leptos-use`** — not raw `Closure` |
| Canvas / Performance / File | raw `web-sys` OK |

Tailwind: only literal class strings in `.rs`/`.css`; computed geometry → `style=`.

## Host gotchas

1. **OpQueue** — serialize open/seek/play-prime (CallSerializer + asyncId). `busy` = draining, not “drop input”.
2. **No `RefCell` across `.await`** — clone `Rc<VideoPlayer>`, await, put back. Engine re-entry: `skills/media.md` RefCell.
3. **`playing` follows transport** — `TransportState::ui_playing(session_live)` (`Playing`, or priming/resume-settle after `begin_play_session`). Never `playing.set(engine.playing)` in `sync_ui`. Seek/Step/Play/Pause/scrub → `dispatch_transport` / `note_transport`.
4. **Transport** — pointers → `VideoRef::scrub_pointer_*`; Seek/Step → `SeekRequested` / `StepRequested`. FSM owns resume (`ScrubRelease { then_play }`). Soft-hold via `pause_for_scrub`. PlayPrime fail → soft-park (no bump) so retry works.
5. **Playhead** — sync on pointerdown/move before `scrub_pointer_*`. `sync_ui` updates playhead only while playing or on new clip. Leaving play: snap playhead to engine. Hold ←/→: aim `engine±1`, not `playhead++`.
6. **Clock / play session** — `VideoPlayer::media_time()` only; engine owns prefetch, underrun, loop seam, silent-clock arm (`play_raf_tick` / `begin_play_session`). Host: OpQueue prime + rAF tick + visibility → hold/resume APIs.
7. **rAF** — `use_raf_fn` resume after prime; throttle `sync_ui`/fps ~10 Hz; call `play_raf_tick` (not host underrun/loop).
8. **Open / clip switch** — file input uses wasm spawn; `retire_clip` before zeroing UI; remote URL → `/proxy` then UrlSource/blob.
9. **`ended` / `close`** — Ended only from engine EOF (loop off); clear on open/seek/play/scrub; `close` = cancel_all + drop player.
10. **Cancelled** — soft (no error banner). Clear sticky `error` on Ok settle.
