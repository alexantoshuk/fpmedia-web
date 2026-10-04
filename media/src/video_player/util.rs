//! WASM event-loop helpers for [`super::VideoPlayer`].
//!
//! Split from `mod.rs` for navigation only — no behavior change (P0).

use wasm_bindgen::JsCast;
use wasm_bindgen::closure::Closure;

use crate::error::MediaError;

pub(super) fn performance_now_ms() -> Option<f64> {
    web_sys::window()?.performance().map(|p| p.now())
}

/// WebCodecs poison after densify/settle abort races (Exact retry once).
///
/// **Named recovery predicate** (not a [`crate::SettlePolicy`] knob) — classifies
/// closed-codec / Decoding error / wedged backpressure so Exact/PlayPrime can
/// soft-cold or recreate once. Includes wedged `decodeQueueSize backpressure
/// timed out` after the reset+retry in [`super::VideoPlayer`] decode (`q≈41,held=0`).
pub(super) fn is_decoder_poison(err: &MediaError) -> bool {
    let MediaError::WebCodecs(msg) = err else {
        return false;
    };
    let m = msg.to_ascii_lowercase();
    m.contains("decoding error")
        || m.contains("closed")
        || m.contains("cannot call")
        || m.contains("key frame required")
        || m.contains("decodequeuesize backpressure timed out")
}

/// One event-loop turn via `Promise.resolve` (microtask).
pub(super) async fn yield_event_loop() {
    let _ = wasm_bindgen_futures::JsFuture::from(js_sys::Promise::resolve(
        &wasm_bindgen::JsValue::UNDEFINED,
    ))
    .await;
}

/// Await one animation frame with a dedicated Closure (not Promise.resolve).
///
/// Used to pace densify between GOPs so Chrome's GPU process can reclaim after
/// a burst of `transferToImageBitmap`. Do not pass `resolve` directly to rAF.
pub(super) async fn wait_animation_frame() {
    let promise = js_sys::Promise::new(&mut |resolve, _reject| {
        let Some(window) = web_sys::window() else {
            let _ = resolve.call0(&wasm_bindgen::JsValue::NULL);
            return;
        };
        let resolve = resolve.clone();
        let cb = Closure::once(move |_t: f64| {
            let _ = resolve.call0(&wasm_bindgen::JsValue::NULL);
        });
        let _ = window.request_animation_frame(cb.as_ref().unchecked_ref());
        cb.forget();
    });
    let _ = wasm_bindgen_futures::JsFuture::from(promise).await;
}

pub(super) async fn sleep_ms(ms: i32) {
    let promise = js_sys::Promise::new(&mut |resolve, _reject| {
        let _ = web_sys::window()
            .expect("window")
            .set_timeout_with_callback_and_timeout_and_arguments_0(&resolve, ms);
    });
    let _ = wasm_bindgen_futures::JsFuture::from(promise).await;
}
