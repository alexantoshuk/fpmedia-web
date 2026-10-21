//! Seek / scrub restart latch (`restarting`).
//!
//! Underrun hold and play render gate on it; WorkloadMode treats `restarting`
//! as Seek. Play loop / session lifetime lives in [`super::play_phase`].
//! See `.ai/architecture.md` § State machines over flags.
//!
//! mediabunny: `examples/media-player/media-player.ts` — `startVideoIterator` on
//! seek; render idles until the new iterator is primed.

use std::fmt;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct SessionModel {
    /// Seek / scrub exact — Workload Seek; no underrun hold.
    pub restarting: bool,
}

impl fmt::Display for SessionModel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.restarting {
            write!(f, "Restarting")
        } else {
            write!(f, "Idle")
        }
    }
}

impl SessionModel {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_idle(self) -> bool {
        !self.restarting
    }

    pub fn is_restarting(self) -> bool {
        self.restarting
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionEvent {
    BeginRestart,
    EndRestart,
}

pub fn reduce(_model: SessionModel, event: SessionEvent) -> SessionModel {
    match event {
        SessionEvent::BeginRestart => SessionModel { restarting: true },
        SessionEvent::EndRestart => SessionModel { restarting: false },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restart_roundtrip() {
        let m = reduce(SessionModel::new(), SessionEvent::BeginRestart);
        assert!(m.is_restarting());
        let m = reduce(m, SessionEvent::EndRestart);
        assert!(m.is_idle());
    }

    #[test]
    fn end_restart_idempotent() {
        let m = reduce(SessionModel::new(), SessionEvent::EndRestart);
        assert!(m.is_idle());
    }

    #[test]
    fn display_labels() {
        assert_eq!(SessionModel::new().to_string(), "Idle");
        assert_eq!(
            reduce(SessionModel::new(), SessionEvent::BeginRestart).to_string(),
            "Restarting"
        );
    }
}
