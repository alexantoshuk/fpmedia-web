//! Tiny async helpers for sync APIs and wasm event-loop yields.

use std::future::Future;
use std::task::{Context, Poll, Waker};

#[cfg(all(target_arch = "wasm32", feature = "webcodecs"))]
use wasm_bindgen::JsCast;

pub fn block_on<F: Future>(fut: F) -> F::Output {
    let waker = Waker::noop();
    let mut cx = Context::from_waker(waker);
    let mut fut = Box::pin(fut);
    for _ in 0..256 {
        match fut.as_mut().poll(&mut cx) {
            Poll::Ready(v) => return v,
            Poll::Pending => continue,
        }
    }
    panic!("block_on: future stayed pending (source not immediately ready?)");
}

/// Yield to the browser event loop so UI / open-timeout can run.
///
/// Sync `re_mp4` parse on wasm otherwise blocks the main thread; use a **macrotask**
/// (`setTimeout(0)`) so Leptos can flush status before the heavy parse, not only a
/// microtask (`Promise.resolve`).
pub async fn yield_now() {
    #[cfg(all(target_arch = "wasm32", feature = "webcodecs"))]
    {
        let promise = js_sys::Promise::new(&mut |resolve, _reject| {
            let window = match web_sys::window() {
                Some(w) => w,
                None => {
                    let _ = resolve.call0(&wasm_bindgen::JsValue::UNDEFINED);
                    return;
                }
            };
            let resolve_ok = resolve.clone();
            let cb = wasm_bindgen::closure::Closure::once_into_js(move || {
                let _ = resolve_ok.call0(&wasm_bindgen::JsValue::UNDEFINED);
            });
            if window
                .set_timeout_with_callback_and_timeout_and_arguments_0(
                    cb.as_ref().unchecked_ref(),
                    0,
                )
                .is_err()
            {
                let _ = resolve.call0(&wasm_bindgen::JsValue::UNDEFINED);
            }
        });
        let _ = wasm_bindgen_futures::JsFuture::from(promise).await;
    }
    #[cfg(not(all(target_arch = "wasm32", feature = "webcodecs")))]
    {
        // Native / tests: no-op.
    }
}
