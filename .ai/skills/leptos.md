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

---

## 0.1 Rendering model (CSR)

The server serves a static `index.html` shell + WASM; `App` mounts via `mount_to_body`. There is **no** SSR HTML render. `HydrationScripts` in `shell()` is cargo-leptos' WASM loader. `frontend::hydrate()` calls `mount_to_body(App)`.

### Mount stability

Long-lived chrome (player shell, providers) must survive route changes — mount **outside** matched parametric route views, or keep a single-page demo without remounting children via `{move || match … => view! { <Child/> }}`.

### Resources in CSR

`resource.get()` in a reactive view closure is fine; show loading UI while `None`. Prefer `LocalResource` for File / fetch / WebPlayer open (no server fn).

---

## Tailwind

Only classes that appear **literally** in `.rs` / `.css` are emitted. Do not build class names with `format!()` for Tailwind utilities; use `style=` for computed geometry.

---

## Player UI gotchas

1. Open / seek / step are **async** and need `&mut WebPlayer` — hold the player behind `Rc<RefCell<…>>` or a single owner task; serialize operations with a `busy` signal.
2. Do not call `pause` / `is_playing` on the Rust player while another async method is awaiting (aliasing). Mirror play state in a Leptos signal for the UI. **Never hold `RefCell`/`borrow_mut` across `.await`** — `take()` the `WebPlayer` out, await, then put it back (otherwise Loop/sync_ui panics: `RefCell already borrowed`).
3. Canvas: pass `HtmlCanvasElement` from `NodeRef` after mount; open player once canvas exists.
4. Status **fps** is measured display rate (presents / wall time over ~1s), not container/`1/duration` — that stays near source fps even when decode lags.
5. **Playback pacing** — AudioContext media clock + `requestAnimationFrame` presents (`present_due`). Not per-frame `setTimeout`.
6. **Prefetch + skip** — prime on Play; each rAF: present due frames (drop overdue), then `prefetch_one` only if queue ≤ 4. Overdue frames are skipped in `present_due`, not via a separate deadline sleep loop.
7. **Play UI cost** — throttle `sync_ui` / fps (~100 ms). No `busy` toggles during the rAF loop.
