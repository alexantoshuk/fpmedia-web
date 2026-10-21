//! Play session phase — replaces `play_session_live` / `play_loop_seam` flags.
//!
//! | Phase      | Render (`present_due`) | `updateNextFrame` overdue draw | Underrun hold |
//! | ---------- | ---------------------- | ------------------------------ | ------------- |
//! | `Idle`     | no                     | no                             | no            |
//! | `Live`     | yes                    | yes                            | yes           |
//! | `LoopSeam` | no                     | no (store as next only)        | no            |
//!
//! mediabunny: `examples/media-player/media-player.ts` — `play()` after
//! `startVideoIterator`; loop = `startVideoIterator(0)` while the clock is held
//! (local adaptation: the example has no loop).

use std::fmt;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum PlayPhase {
    /// No play session (paused / priming / teardown).
    #[default]
    Idle,
    /// PlayPrime done — render presents `nextFrame`, refill runs.
    Live,
    /// Iterator restart at frame 0 under a soft-held clock.
    LoopSeam,
}

impl PlayPhase {
    /// Session started and not torn down (host may treat UI as playing).
    pub fn is_live(self) -> bool {
        matches!(self, Self::Live | Self::LoopSeam)
    }

    /// Render may present due frames / run underrun hold.
    pub fn presents(self) -> bool {
        self == Self::Live
    }
}

impl fmt::Display for PlayPhase {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::Idle => "Idle",
            Self::Live => "Live",
            Self::LoopSeam => "LoopSeam",
        };
        f.write_str(s)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlayPhaseEvent {
    /// `begin_play_session` after PlayPrime.
    Begin,
    /// Render hit EOF while looping.
    LoopStart,
    /// New iterator primed at frame 0 (or the restart was cancelled).
    LoopDone,
    /// Pause / soft-hold / teardown.
    End,
}

pub fn reduce(phase: PlayPhase, event: PlayPhaseEvent) -> PlayPhase {
    match (phase, event) {
        (_, PlayPhaseEvent::Begin) => PlayPhase::Live,
        (_, PlayPhaseEvent::End) => PlayPhase::Idle,
        (PlayPhase::Live, PlayPhaseEvent::LoopStart) => PlayPhase::LoopSeam,
        (PlayPhase::LoopSeam, PlayPhaseEvent::LoopDone) => PlayPhase::Live,
        (p, _) => p,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn begin_loop_roundtrip() {
        let p = reduce(PlayPhase::Idle, PlayPhaseEvent::Begin);
        assert!(p.presents());
        let p = reduce(p, PlayPhaseEvent::LoopStart);
        assert_eq!(p, PlayPhase::LoopSeam);
        assert!(p.is_live() && !p.presents());
        let p = reduce(p, PlayPhaseEvent::LoopDone);
        assert_eq!(p, PlayPhase::Live);
    }

    #[test]
    fn end_mid_seam_stays_idle_on_late_done() {
        let p = reduce(PlayPhase::Live, PlayPhaseEvent::LoopStart);
        let p = reduce(p, PlayPhaseEvent::End);
        assert_eq!(reduce(p, PlayPhaseEvent::LoopDone), PlayPhase::Idle);
    }

    #[test]
    fn loop_start_ignored_when_idle() {
        assert_eq!(
            reduce(PlayPhase::Idle, PlayPhaseEvent::LoopStart),
            PlayPhase::Idle
        );
    }
}
