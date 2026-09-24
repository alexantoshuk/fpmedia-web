//! Play-loop session latches (`restarting` / `preparing_loop`).
//!
//! Replaces free `Cell<bool>` pairs on [`crate::VideoPlayer`]. Prefetch and
//! underrun hold gate on these; WorkloadMode treats `restarting` as Seek.
//! See `.ai/architecture.md` § State machines over flags.
//!
//! The two latches are **orthogonal** (seek can begin while loop-prime fills).
//! Not a single exclusive phase enum — overlapping was already possible with
//! the old bools; preserve that.
//!
//! mediabunny: `examples/media-player/media-player.ts` — `startVideoIterator` on
//! play/seek; pause stops iterators. Path match: seek/cold loop owns decode →
//! present/prefetch idle until restart ends. **No equivalent** for
//! `preparing_loop` (near-EOF prime at review rates) — do **not** set
//! `restarting` during prepare (freezes underrun hold / stalls tail present).

use std::fmt;

/// Play-session gates for prefetch / densify competition.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct SessionModel {
    /// Seek / scrub exact / cold loop wrap — Workload Seek; no underrun hold.
    pub restarting: bool,
    /// Near-EOF loop prime fill — blocks prefetch only (not Workload Seek).
    pub preparing_loop: bool,
}

impl fmt::Display for SessionModel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match (self.restarting, self.preparing_loop) {
            (false, false) => write!(f, "Idle"),
            (true, false) => write!(f, "Restarting"),
            (false, true) => write!(f, "PreparingLoop"),
            (true, true) => write!(f, "Restarting+PreparingLoop"),
        }
    }
}

impl SessionModel {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_idle(self) -> bool {
        !self.restarting && !self.preparing_loop
    }

    pub fn is_restarting(self) -> bool {
        self.restarting
    }

    pub fn is_preparing_loop(self) -> bool {
        self.preparing_loop
    }

    /// Prefetch / `wants_play_prefetch` must idle.
    pub fn blocks_prefetch(self) -> bool {
        self.restarting || self.preparing_loop
    }
}

/// Input to [`reduce`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionEvent {
    BeginRestart,
    EndRestart,
    BeginPrepareLoop,
    EndPrepareLoop,
}

/// Pure latch update (no side-effect cmds — callers own decode/prime work).
pub fn reduce(model: SessionModel, event: SessionEvent) -> SessionModel {
    match event {
        SessionEvent::BeginRestart => SessionModel {
            restarting: true,
            preparing_loop: model.preparing_loop,
        },
        SessionEvent::EndRestart => SessionModel {
            restarting: false,
            preparing_loop: model.preparing_loop,
        },
        SessionEvent::BeginPrepareLoop => SessionModel {
            restarting: model.restarting,
            preparing_loop: true,
        },
        SessionEvent::EndPrepareLoop => SessionModel {
            restarting: model.restarting,
            preparing_loop: false,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restart_roundtrip_preserves_prepare() {
        let m = reduce(SessionModel::new(), SessionEvent::BeginPrepareLoop);
        assert!(m.is_preparing_loop());
        let m = reduce(m, SessionEvent::BeginRestart);
        assert!(m.restarting && m.preparing_loop);
        assert!(m.blocks_prefetch());
        let m = reduce(m, SessionEvent::EndRestart);
        assert!(!m.restarting && m.preparing_loop);
        let m = reduce(m, SessionEvent::EndPrepareLoop);
        assert!(m.is_idle());
    }

    #[test]
    fn prepare_does_not_set_restarting() {
        let m = reduce(SessionModel::new(), SessionEvent::BeginPrepareLoop);
        assert!(!m.is_restarting());
        assert!(m.blocks_prefetch());
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
        assert_eq!(
            reduce(SessionModel::new(), SessionEvent::BeginPrepareLoop).to_string(),
            "PreparingLoop"
        );
    }
}
