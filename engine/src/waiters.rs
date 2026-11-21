//! Park / wake list for Promise resolvers (WebCodecs decoder + audio output).
//!
//! A consumer parks by [`Waiters::register`]-ing the `resolve` of a `Promise`
//! it races against a timer; a producer callback calls
//! [`Waiters::wake_deferred`]. Wake rules (which callback wakes which list) stay
//! at the call sites — see the mediabunny cites in `decode/webcodecs/core.rs`.

use std::cell::RefCell;
use std::rc::Rc;

use wasm_bindgen::JsValue;
use wasm_bindgen_futures::spawn_local;

/// `Clone` handle over one resolver list (clones share the list).
#[derive(Clone, Default)]
pub(crate) struct Waiters(Rc<RefCell<Vec<js_sys::Function>>>);

impl Waiters {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Park a Promise `resolve` until the next [`Self::wake_deferred`].
    pub(crate) fn register(&self, resolve: js_sys::Function) {
        self.0.borrow_mut().push(resolve);
    }

    /// Drop parked resolvers without waking (timeout path: Promise abandoned).
    pub(crate) fn clear(&self) {
        self.0.borrow_mut().clear();
    }

    /// Take every parked resolver and resolve it from a fresh task.
    ///
    /// Leaves the WebCodecs / audio callback stack first (reentrancy: a sync
    /// wake re-enters Rust from inside the `Closure` and panics with "closure
    /// invoked recursively or after being dropped"). `spawn_local` is reliable;
    /// raw `queueMicrotask(promiseResolve)` can fail to run and leave `open` /
    /// decode stuck with `busy=true` and no error.
    pub(crate) fn wake_deferred(&self) {
        let waiting = std::mem::take(&mut *self.0.borrow_mut());
        for resolve in waiting {
            spawn_local(async move {
                let _ = resolve.call0(&JsValue::NULL);
            });
        }
    }
}
