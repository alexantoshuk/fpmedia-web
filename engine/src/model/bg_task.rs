//! Single-flight background work: at most one running task + one pending restart.
//!
//! Used three times (densify / gray FetchMap / thumb key-fill) — planes stay
//! independent so gray never blocks Exact on `scrub_busy`.
//!
//! `mediabunny: src/media-sink.ts` asyncIterator `return()` — one active retrieve
//! pass; no densify/FetchMap/thumb planes there (local invention for those).

use std::cell::Cell;

/// Busy flag + optional "run again when free" latch.
#[derive(Debug, Default)]
pub struct SingleFlight {
    busy: Cell<bool>,
    pending: Cell<bool>,
}

impl SingleFlight {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_busy(&self) -> bool {
        self.busy.get()
    }

    pub fn is_pending(&self) -> bool {
        self.pending.get()
    }

    pub fn latch_pending(&self) {
        self.pending.set(true);
    }

    pub fn set_pending(&self, pending: bool) {
        self.pending.set(pending);
    }

    pub fn clear_pending(&self) {
        self.pending.set(false);
    }

    /// Take pending without touching busy (thumb fill restart check).
    pub fn take_pending(&self) -> bool {
        self.pending.replace(false)
    }

    /// Start if idle (clears pending). If busy, latch pending and return `false`.
    ///
    /// Densify kick / thumb bg fill.
    pub fn request(&self) -> bool {
        if self.busy.get() {
            self.pending.set(true);
            return false;
        }
        self.busy.set(true);
        self.pending.set(false);
        true
    }

    /// Start if idle. If busy, return `false` without latching pending.
    ///
    /// Gray hole-fill (schedule is a no-op while running) / hover decode claim.
    pub fn try_claim(&self) -> bool {
        if self.busy.get() {
            return false;
        }
        self.busy.set(true);
        true
    }

    /// Mark busy and clear pending (caller already observed idle).
    pub fn begin(&self) {
        debug_assert!(!self.busy.get(), "SingleFlight::begin while busy");
        self.busy.set(true);
        self.pending.set(false);
    }

    /// Clear busy; leave pending (ExactHold — respawn after settle).
    pub fn release_busy(&self) {
        self.busy.set(false);
    }

    /// Clear busy and take pending (`true` → caller may respawn).
    pub fn finish(&self) -> bool {
        self.busy.set(false);
        self.pending.replace(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_latches_when_busy() {
        let f = SingleFlight::new();
        assert!(f.request());
        assert!(f.is_busy());
        assert!(!f.is_pending());
        assert!(!f.request());
        assert!(f.is_pending());
        assert!(f.finish());
        assert!(!f.is_busy());
        assert!(!f.is_pending());
    }

    #[test]
    fn try_claim_skips_without_latch() {
        let f = SingleFlight::new();
        assert!(f.try_claim());
        assert!(!f.try_claim());
        assert!(!f.is_pending());
        f.release_busy();
        assert!(f.try_claim());
    }

    #[test]
    fn release_busy_keeps_pending() {
        let f = SingleFlight::new();
        assert!(f.request());
        f.latch_pending();
        f.release_busy();
        assert!(!f.is_busy());
        assert!(f.is_pending());
        assert!(f.finish()); // take leftover pending after a no-op run edge
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
