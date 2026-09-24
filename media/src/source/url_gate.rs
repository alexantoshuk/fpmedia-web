//! Concurrent Range GET gate for [`UrlSource`](super::UrlSource).
//!
//! mediabunny: `src/source.ts` `UrlSource` `DEFAULT_PARALLELISM = 2` →
//! `ReadOrchestrator.maxWorkerCount`. Workers there stream open-ended Ranges;
//! here we only limit how many closed `bytes=start-end` GETs run at once
//! (byte cache / network prefetch live in [`super::CachedSource`]).

// Constructed from wasm `UrlSource`; native lib keeps the type for unit tests.
#![cfg_attr(
    not(all(target_arch = "wasm32", feature = "webcodecs")),
    allow(dead_code)
)]

use std::sync::{Arc, Mutex};

/// mediabunny `UrlSource` default `parallelism` / `maxWorkerCount`.
pub const DEFAULT_URL_PARALLELISM: usize = 2;

/// Progressive-open path: match demux `MFRA_MOOF_FETCH_PARALLEL` so moof waves
/// are not serialized behind the play-time default of 2.
pub const OPEN_URL_PARALLELISM: usize = 4;

/// Shared slot limiter — clones of [`super::UrlSource`] share one gate.
#[derive(Clone, Debug)]
pub struct UrlFetchGate {
    inner: Arc<Mutex<GateState>>,
}

#[derive(Debug)]
struct GateState {
    max: usize,
    active: usize,
}

impl UrlFetchGate {
    pub fn new(parallelism: usize) -> Self {
        Self {
            inner: Arc::new(Mutex::new(GateState {
                max: parallelism.max(1),
                active: 0,
            })),
        }
    }

    pub fn max(&self) -> usize {
        self.inner.lock().expect("UrlFetchGate").max
    }

    /// Run `fut` while holding one worker slot (released on drop / cancel).
    pub async fn with_slot<T>(&self, fut: impl std::future::Future<Output = T>) -> T {
        self.acquire().await;
        let _guard = SlotGuard(self);
        fut.await
    }

    async fn acquire(&self) {
        loop {
            {
                let mut g = self.inner.lock().expect("UrlFetchGate");
                if g.active < g.max {
                    g.active += 1;
                    return;
                }
            }
            // Contended: yield so other GETs / the UI can progress (wasm macrotask).
            crate::util::yield_now().await;
        }
    }

    fn release(&self) {
        let mut g = self.inner.lock().expect("UrlFetchGate");
        debug_assert!(g.active > 0, "UrlFetchGate release with active=0");
        g.active = g.active.saturating_sub(1);
    }
}

struct SlotGuard<'a>(&'a UrlFetchGate);

impl Drop for SlotGuard<'_> {
    fn drop(&mut self) {
        self.0.release();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gate_acquire_release_respects_max() {
        let gate = UrlFetchGate::new(2);
        crate::util::block_on(async {
            gate.acquire().await;
            gate.acquire().await;
            assert_eq!(gate.inner.lock().unwrap().active, 2);
            gate.release();
            assert_eq!(gate.inner.lock().unwrap().active, 1);
            gate.acquire().await;
            assert_eq!(gate.inner.lock().unwrap().active, 2);
            gate.release();
            gate.release();
            assert_eq!(gate.inner.lock().unwrap().active, 0);
        });
    }

    #[test]
    fn with_slot_releases_on_completion() {
        let gate = UrlFetchGate::new(1);
        crate::util::block_on(async {
            let v = gate.with_slot(async { 7u32 }).await;
            assert_eq!(v, 7);
            assert_eq!(gate.inner.lock().unwrap().active, 0);
        });
    }

    #[test]
    fn parallelism_at_least_one() {
        let g = UrlFetchGate::new(0);
        assert_eq!(g.max(), 1);
    }
}
