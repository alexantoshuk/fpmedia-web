//! Browser event-loop timers shared by the engine and hosts.
//!
//! Callbacks resolve through a dedicated Rust closure — never hand a bare
//! Promise `resolve` to rAF / `setTimeout`. Without a `window` every wait
//! resolves immediately. mediabunny: `src/misc.ts` `wait` / `rejectAfter`.

use js_sys::Promise;
use wasm_bindgen::closure::Closure;
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;

/// `performance.now()` in ms, or `None` without `window` / `performance`.
pub fn now_ms() -> Option<f64> {
    web_sys::window()?.performance().map(|p| p.now())
}

/// Promise resolved with `on_fire()` after `ms` (macrotask). If the timer
/// cannot be armed, `on_fire` runs and the promise resolves immediately.
pub fn timeout_promise(ms: i32, on_fire: impl FnOnce() -> JsValue + 'static) -> Promise {
    let mut on_fire = Some(on_fire);
    Promise::new(&mut |resolve, _reject| {
        let Some(fire) = on_fire.take() else {
            return;
        };
        let Some(window) = web_sys::window() else {
            let _ = resolve.call1(&JsValue::NULL, &fire());
            return;
        };
        let fired = std::rc::Rc::new(std::cell::Cell::new(Some(fire)));
        let fired_cb = fired.clone();
        let resolve_cb = resolve.clone();
        let cb = Closure::once_into_js(move || {
            if let Some(fire) = fired_cb.take() {
                let _ = resolve_cb.call1(&JsValue::NULL, &fire());
            }
        });
        if window
            .set_timeout_with_callback_and_timeout_and_arguments_0(cb.unchecked_ref(), ms)
            .is_err()
            && let Some(fire) = fired.take()
        {
            let _ = resolve.call1(&JsValue::NULL, &fire());
        }
    })
}

/// Sleep `ms` via `setTimeout` (macrotask).
pub async fn sleep_ms(ms: i32) {
    let _ = JsFuture::from(timeout_promise(ms, || JsValue::UNDEFINED)).await;
}

/// One macrotask turn (`setTimeout(0)`) — lets the browser render / run timers.
pub async fn yield_macrotask() {
    sleep_ms(0).await;
}

/// One microtask turn (`Promise.resolve`). Does not let the browser paint.
pub async fn yield_microtask() {
    let _ = JsFuture::from(Promise::resolve(&JsValue::UNDEFINED)).await;
}

/// Await the next animation frame.
pub async fn next_animation_frame() {
    let promise = Promise::new(&mut |resolve, _reject| {
        let Some(window) = web_sys::window() else {
            let _ = resolve.call0(&JsValue::NULL);
            return;
        };
        let resolve_cb = resolve.clone();
        let cb = Closure::once_into_js(move |_t: f64| {
            let _ = resolve_cb.call0(&JsValue::NULL);
        });
        if window.request_animation_frame(cb.unchecked_ref()).is_err() {
            let _ = resolve.call0(&JsValue::NULL);
        }
    });
    let _ = JsFuture::from(promise).await;
}
