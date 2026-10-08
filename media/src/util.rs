//! Tiny async helpers for sync APIs and wasm event-loop yields.

use std::future::Future;
use std::task::{Context, Poll, Waker};
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
    crate::timer::yield_macrotask().await;
    #[cfg(not(all(target_arch = "wasm32", feature = "webcodecs")))]
    {
        // Native / tests: no-op.
    }
}
