//! Copy handle over an engine [`Epoch`] stored in Leptos local storage.
//!
//! One cancel-counter shape for host code that captures a [`Ticket`] before an
//! `.await` and checks it after (playlist `open_gen`, hover-thumb paint).
//! Same semantics as the engine type — see `architecture.md` § Ownership & types.

use fpmedia_web::{Epoch, Ticket};
use leptos::prelude::*;

/// `Copy` handle (arena-owned [`Epoch`]); clones share the counter.
#[derive(Clone, Copy)]
pub(crate) struct LocalEpoch(StoredValue<Epoch, LocalStorage>);

impl LocalEpoch {
    pub(crate) fn new() -> Self {
        Self(StoredValue::new_local(Epoch::new()))
    }

    /// Ticket for the current epoch.
    pub(crate) fn ticket(self) -> Ticket {
        self.0.with_value(Epoch::ticket)
    }

    /// Retire every outstanding ticket; returns the new epoch's ticket.
    pub(crate) fn bump(self) -> Ticket {
        self.0.with_value(Epoch::bump)
    }

    /// `true` while `ticket` has not been retired by [`Self::bump`].
    pub(crate) fn is_current(self, ticket: Ticket) -> bool {
        self.0.with_value(|e| e.is_current(ticket))
    }
}
