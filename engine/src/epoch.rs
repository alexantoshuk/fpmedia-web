//! Cancel / stale-work counter: [`Epoch`] hands out [`Ticket`]s, `bump` retires them.
//!
//! Async work captures a [`Ticket`] at start and checks
//! [`Epoch::is_current`] after every `.await`; whoever invalidates that work
//! (reset, detach, new seek, Drop) calls [`Epoch::bump`]. A ticket is a
//! distinct type, so it cannot be compared against a frame count, a PTS or
//! another counter's value. Single-threaded (`Cell`); shared owners hold
//! `Rc<Epoch>`.

use std::cell::Cell;

/// Snapshot of an [`Epoch`]; stale once the epoch has been bumped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ticket(u64);

/// Monotonic (wrapping) invalidation counter.
#[derive(Debug, Default)]
pub struct Epoch(Cell<u64>);

impl Epoch {
    pub fn new() -> Self {
        Self::default()
    }

    /// Ticket for the current epoch.
    pub fn ticket(&self) -> Ticket {
        Ticket(self.0.get())
    }

    /// Retire every outstanding ticket; returns a ticket for the new epoch.
    pub fn bump(&self) -> Ticket {
        self.0.set(self.0.get().wrapping_add(1));
        self.ticket()
    }

    /// `true` while `ticket` has not been retired by [`Self::bump`].
    pub fn is_current(&self, ticket: Ticket) -> bool {
        self.0.get() == ticket.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bump_retires_old_tickets() {
        let e = Epoch::new();
        let a = e.ticket();
        assert!(e.is_current(a));
        let b = e.bump();
        assert!(!e.is_current(a));
        assert!(e.is_current(b));
        assert_eq!(e.ticket(), b);
    }

    #[test]
    fn wraps_without_panic() {
        let e = Epoch(Cell::new(u64::MAX));
        let a = e.ticket();
        let b = e.bump();
        assert!(!e.is_current(a));
        assert!(e.is_current(b));
    }
}
