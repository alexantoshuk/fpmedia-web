//! Single-flight background work: at most one running task + one pending restart.
//!
//! Used for densify / gray FetchMap / thumb key-fill / play-origin warm planes —
//! planes stay independent so gray never blocks Exact on `scrub_busy`.
//!
//! Ownership: [`SingleFlight::request`] / [`SingleFlight::try_claim`] return a
//! [`FlightGuard`]; the guard lives as long as the task and releases busy in
//! `Drop` (early `return`, error, cancelled future). [`FlightGuard::finish`] is
//! the normal exit: busy off **and** take pending.
//!
//! `mediabunny: src/media-sink.ts` asyncIterator `return()` — one active retrieve
//! pass released when the iterator ends; no densify/FetchMap/thumb planes there
//! (local invention for those).

use std::cell::Cell;
use std::rc::Rc;

use crate::epoch::{Epoch, Ticket};

#[derive(Debug, Default)]
struct Inner {
    busy: Cell<bool>,
    pending: Cell<bool>,
    /// Retires the live guard when busy is force-released (or the guard ends),
    /// so a stalled task's late `Drop` cannot clear a newer owner's busy.
    owner: Epoch,
}

/// `Clone` handle: busy flag + optional "run again when free" latch.
#[derive(Clone, Debug, Default)]
pub struct SingleFlight(Rc<Inner>);

/// Proof of the running flight. `Drop` = [`SingleFlight::force_release`] without
/// touching pending, unless a newer owner already took over.
#[must_use = "dropping the guard releases the flight immediately"]
#[derive(Debug)]
pub struct FlightGuard {
    flight: SingleFlight,
    ticket: Ticket,
}

impl SingleFlight {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_busy(&self) -> bool {
        self.0.busy.get()
    }

    pub fn is_pending(&self) -> bool {
        self.0.pending.get()
    }

    pub fn latch_pending(&self) {
        self.0.pending.set(true);
    }

    pub fn set_pending(&self, pending: bool) {
        self.0.pending.set(pending);
    }

    pub fn clear_pending(&self) {
        self.0.pending.set(false);
    }

    /// Take pending without touching busy (thumb fill restart check).
    pub fn take_pending(&self) -> bool {
        self.0.pending.replace(false)
    }

    fn claim(&self) -> FlightGuard {
        self.0.busy.set(true);
        FlightGuard {
            flight: self.clone(),
            ticket: self.0.owner.bump(),
        }
    }

    /// Start if idle (clears pending). If busy, latch pending and return `None`.
    ///
    /// Densify kick / thumb bg fill / origin warm.
    pub fn request(&self) -> Option<FlightGuard> {
        if self.0.busy.get() {
            self.0.pending.set(true);
            return None;
        }
        self.0.pending.set(false);
        Some(self.claim())
    }

    /// Start if idle. If busy, return `None` without latching pending.
    ///
    /// Gray hole-fill (schedule is a no-op while running) / hover decode claim.
    pub fn try_claim(&self) -> Option<FlightGuard> {
        if self.0.busy.get() {
            return None;
        }
        Some(self.claim())
    }

    /// Re-arm right after [`FlightGuard::finish`] reported pending (caller
    /// already observed idle): busy on, pending cleared, fresh guard.
    pub fn begin(&self) -> FlightGuard {
        debug_assert!(!self.0.busy.get(), "SingleFlight::begin while busy");
        self.0.pending.set(false);
        self.claim()
    }

    /// Clear busy from outside the task (settle/seek yield gave up on a stalled
    /// task, hard clear). Leaves pending; retires the live guard so its late
    /// `Drop` / `finish` cannot clobber a newer owner.
    pub fn force_release(&self) {
        self.0.busy.set(false);
        self.0.owner.bump();
    }
}

impl FlightGuard {
    /// Normal exit: busy off + take pending (`true` → caller may respawn).
    ///
    /// A guard retired by [`SingleFlight::force_release`] leaves busy alone (a
    /// newer owner may hold it) and takes pending only while idle.
    pub fn finish(self) -> bool {
        let inner = &self.flight.0;
        if inner.owner.is_current(self.ticket) {
            inner.busy.set(false);
            inner.owner.bump();
            inner.pending.replace(false)
        } else if !inner.busy.get() {
            inner.pending.replace(false)
        } else {
            false
        }
    }
}

impl Drop for FlightGuard {
    /// Clear busy; leave pending (ExactHold — respawn after settle).
    fn drop(&mut self) {
        let inner = &self.flight.0;
        if inner.owner.is_current(self.ticket) {
            inner.busy.set(false);
            inner.owner.bump();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_latches_when_busy() {
        let f = SingleFlight::new();
        let g = f.request().expect("idle");
        assert!(f.is_busy());
        assert!(!f.is_pending());
        assert!(f.request().is_none());
        assert!(f.is_pending());
        assert!(g.finish());
        assert!(!f.is_busy());
        assert!(!f.is_pending());
    }

    #[test]
    fn try_claim_skips_without_latch() {
        let f = SingleFlight::new();
        let g = f.try_claim().expect("idle");
        assert!(f.try_claim().is_none());
        assert!(!f.is_pending());
        drop(g);
        assert!(f.try_claim().is_some());
    }

    #[test]
    fn drop_releases_busy_keeps_pending() {
        let f = SingleFlight::new();
        let g = f.request().expect("idle");
        f.latch_pending();
        drop(g);
        assert!(!f.is_busy());
        assert!(f.is_pending());
        let g = f.request().expect("idle");
        assert!(!f.is_pending());
        assert!(!g.finish());
    }

    #[test]
    fn force_release_retires_stale_guard() {
        let f = SingleFlight::new();
        let stale = f.request().expect("idle");
        f.force_release();
        assert!(!f.is_busy());
        let fresh = f.try_claim().expect("idle after force");
        drop(stale); // must not clear the new owner's busy
        assert!(f.is_busy());
        drop(fresh);
        assert!(!f.is_busy());
    }

    #[test]
    fn stale_finish_leaves_new_owner_and_pending() {
        let f = SingleFlight::new();
        let stale = f.request().expect("idle");
        f.force_release();
        let fresh = f.try_claim().expect("idle after force");
        f.latch_pending();
        assert!(!stale.finish());
        assert!(f.is_busy());
        assert!(f.is_pending());
        drop(fresh);
    }

    #[test]
    fn stale_finish_when_idle_takes_pending() {
        let f = SingleFlight::new();
        let stale = f.request().expect("idle");
        f.latch_pending();
        f.force_release();
        assert!(stale.finish());
        assert!(!f.is_pending());
    }

    #[test]
    fn begin_rearms_after_finish() {
        let f = SingleFlight::new();
        let g = f.request().expect("idle");
        f.latch_pending();
        assert!(g.finish());
        let g2 = f.begin();
        assert!(f.is_busy());
        assert!(!f.is_pending());
        drop(g2);
        assert!(!f.is_busy());
    }

    #[test]
    fn abort_rearm_via_set_pending() {
        let f = SingleFlight::new();
        f.set_pending(true);
        assert!(f.is_pending());
        f.set_pending(false);
        assert!(!f.is_pending());
    }
}
