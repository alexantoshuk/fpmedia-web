# Leptos 0.8.x skill notes

> Read before touching any component, resource, or routing code.
> Project pins **Leptos 0.8.x**. 0.6/0.7 examples **won't compile**.

---

## 0. Quick reference

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

Prefer `leptos-use` whenever it covers the Web API. Raw `web-sys` in `video/` for media plumbing (`AudioContext`, canvas); in `app/` only for demo File / input element types.

---

## 0.1 Rendering model (CSR)

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

1. Open / seek / step are **async** and need `&mut WebPlayer` — hold the player behind `Rc<RefCell<…>>` or a single owner task; serialize operations with a `busy` signal.
2. Do not call `pause` / `is_playing` on the Rust player while another async method is awaiting (aliasing). Mirror play state in a Leptos signal for the UI. **Never hold `RefCell`/`borrow_mut` across `.await`** — `take()` the `WebPlayer` out, await, then put it back (otherwise Loop/sync_ui panics: `RefCell already borrowed`). Seek/step/open must use `take()`.
3. Canvas: pass `HtmlCanvasElement` from `NodeRef` after mount; open player once canvas exists.
4. Status **fps** is measured display rate (presents / wall time over ~1s), not container/`1/duration` — that stays near source fps even when decode lags. Source fps / codec / size come from `WebPlayer::metadata()` (shown in demo `.wm-meta`).
5. **Playback pacing** — AudioContext media clock + `requestAnimationFrame` presents (`present_due`). Not per-frame `setTimeout`. **Set clock origins after `prime_playback`** — priming 1080p/B-frame clips can take hundreds of ms; starting the clock beforehand makes `present_due` skip immediately (start stutter, low measured fps).
6. **Prefetch + skip** — prime on Play; background pump fills until `PRESENTATION_QUEUE_CAP` (not a lower threshold). Overdue frames are skipped in `present_due`.
7. **Play UI cost** — throttle `sync_ui` / fps (~100 ms). No `busy` toggles during the rAF loop.
8. **Viewport layout** — `.wm-viewport` is a fixed media stage; canvas uses contain (`max-width/max-height: 100%` + source `aspect-ratio`). Call `WebPlayer::relayout()` after open; `use_resize_observer` on the viewport NodeRef also calls `relayout` (try_borrow — skip if player RefCell is busy).
9. **`leptos-use` playback** — present loop = `use_raf_fn_with_options(..., immediate: false)`; `resume()` only **after** `prime_playback` + media-clock origins. Pause/EOF clears a shared `playing` signal; an `Effect` calls `pause()` on the Pausable. Async `loop_to_start` sets a session `restarting` flag so rAF skips until origins reset. Prefetch stays a separate `spawn_local` with a thin await-rAF helper — `use_raf_fn` is callback-oriented and cannot `.await` WebCodecs.
10. **Loop clock** — reset origins only **after** `loop_to_start`. Prefer `prepare_loop_if_needed` once EOF is queued (same decoder → `loop_prime`) so the seam is a swap; cold restart only if prepare missed.
11. **`VideoRef` ops** — non-`Send` callbacks live in `StoredValue::new_local`; do not put `Rc<dyn Fn>` in a default `RwSignal` / `StoredValue` (needs `Send + Sync`).
12. **Cancel / epochs** — treat `MediaError::Cancelled` as soft (no error banner). `open_epoch` / `play_epoch` discard late open/prime. `AudioContext.close()` when clearing the play session. Prefetch rAF: pass Promise `resolve` to `requestAnimationFrame` (no `Closure::forget`).
13. **Next (not done):** one command queue for open / seek / play-prime + abort on epoch — mediabunny player `asyncId` + sink `.return()`, library `CallSerializer` (promise chain). `leptos-use` does not cover this; see `.ai/state.md` → Video host / races.
