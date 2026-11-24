//! Media-timeline phase (run / stop / underrun hold / scrub hold) + anchors bag.
//!
//! [`ClockModel`] packs phase with hold/resume/silent/catch-up anchors so
//! Stopped + held origin is cleared on Stop / ResetForPlay (not a parallel cell
//! soup). See `.ai/architecture.md` § State machines over flags.
//!
//! mediabunny: `examples/media-player/media-player.ts` — `getPlaybackTime`, `play`,
//! `pause`, `seekToTime` (pause → snap → play). Path match: audio/`AudioContext`
//! drives time while playing; pause snaps origin. **No equivalent** for underrun
//! hold or scrub soft-hold — local adaptations; do not change those policies here.
//! Tab-hidden: no clock hold (MB continues); host yields scrub planes only.

use std::fmt;

/// Whether the media timeline is advancing and why it is frozen.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ClockState {
    /// Hard pause / open / before first play — cold [`crate::VideoPlayer::start_media_clock`].
    #[default]
    Stopped,
    /// `media_time` advances (wall or `AudioContext`).
    Running,
    /// Play alive, presentation empty — freeze without skip spiral.
    HoldUnderrun,
    /// Scrub-while-play soft silence — warm [`crate::VideoPlayer::resume_media_clock`].
    HoldScrub,
}

impl fmt::Display for ClockState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Stopped => write!(f, "Stopped"),
            Self::Running => write!(f, "Running"),
            Self::HoldUnderrun => write!(f, "HoldUnderrun"),
            Self::HoldScrub => write!(f, "HoldScrub"),
        }
    }
}

impl ClockState {
    pub fn is_running(self) -> bool {
        matches!(self, Self::Running)
    }

    pub fn is_holding(self) -> bool {
        matches!(self, Self::HoldUnderrun | Self::HoldScrub)
    }

    /// Bg sync-verify / backfill must yield while underrun owns the network.
    pub fn underrun_holding(self) -> bool {
        matches!(self, Self::HoldUnderrun)
    }

    pub fn scrub_holding(self) -> bool {
        matches!(self, Self::HoldScrub)
    }

    /// Pause deferred bitstream sync-verify while underrun or scrub soft-hold
    /// own the network.
    pub fn blocks_bg_sync_verify(self) -> bool {
        self.underrun_holding() || self.scrub_holding()
    }
}

/// Input to [`reduce`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClockEvent {
    /// Cold play begin after hard pause / open.
    Start,
    /// Warm resume after underrun / scrub soft-hold / loop seam.
    Resume,
    /// Hard pause / open / EOF.
    Stop,
    /// Presentation empty while next frame is due.
    HoldUnderrun,
    /// Leave play for timeline scrub (soft audio).
    HoldScrub,
    /// New [`crate::VideoPlayer::play`] — drop in-progress underrun timer without
    /// accumulating (same as clearing [`ClockModel::underrun_since_ms`]).
    ResetForPlay,
}

/// Phase + soft-hold / underrun / PlayPrime anchors (`Copy` bag).
///
/// After [`ClockEvent::Stop`] / [`ClockEvent::ResetForPlay`], session anchors
/// ([`Self::held_origin`], [`Self::resuming`], [`Self::pending_silent`],
/// [`Self::underrun_since_ms`]) are cleared. Catch-up grace may outlive a brief
/// phase flip (same as pre-bag behaviour).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ClockModel {
    pub phase: ClockState,
    /// Soft-hold / underrun / seam snap origin (secs).
    pub held_origin: Option<f64>,
    /// Wall stamp while `phase == HoldUnderrun`.
    pub underrun_since_ms: Option<f64>,
    /// PlayPrime: arm silent Start on next present tick.
    pub pending_silent: bool,
    /// `resume_media_clock` in flight — do not stack.
    pub resuming: bool,
    /// Tab-visible catch-up grace (no clock hold while the document was hidden).
    /// PlayPrime does not arm this — a stall would fast-forward overdue frames.
    pub catchup_until_ms: Option<f64>,
}

impl ClockModel {
    pub fn phase(self) -> ClockState {
        self.phase
    }

    /// Clear hold/resume/silent/underrun-stamp anchors (Stop / ResetForPlay /
    /// play-session end). Leaves [`Self::catchup_until_ms`] alone.
    pub fn clear_session_anchors(&mut self) {
        self.held_origin = None;
        self.underrun_since_ms = None;
        self.pending_silent = false;
        self.resuming = false;
    }
}

/// Side effects for [`crate::VideoPlayer`] underrun stats.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClockCmd {
    /// Enter underrun hold — bump count + stamp `performance.now`.
    NoteUnderrunHoldStart,
    /// Leave underrun hold — add elapsed wall ms to totals.
    FinishUnderrunTimer,
    /// Abandon in-progress underrun stamp without adding to totals.
    AbandonUnderrunTimer,
}

/// Pure transition.
pub fn reduce(state: ClockState, event: ClockEvent) -> (ClockState, Vec<ClockCmd>) {
    use ClockCmd as C;
    use ClockEvent as E;
    use ClockState as S;

    match event {
        E::Start | E::Resume => {
            let mut cmds = Vec::new();
            if state == S::HoldUnderrun {
                cmds.push(C::FinishUnderrunTimer);
            }
            (S::Running, cmds)
        }

        E::Stop => {
            let mut cmds = Vec::new();
            if state == S::HoldUnderrun {
                cmds.push(C::FinishUnderrunTimer);
            }
            (S::Stopped, cmds)
        }

        E::HoldUnderrun => {
            let mut cmds = Vec::new();
            if state != S::HoldUnderrun {
                cmds.push(C::NoteUnderrunHoldStart);
            }
            (S::HoldUnderrun, cmds)
        }

        E::HoldScrub => {
            let mut cmds = Vec::new();
            if state == S::HoldUnderrun {
                cmds.push(C::FinishUnderrunTimer);
            }
            (S::HoldScrub, cmds)
        }

        E::ResetForPlay => {
            let mut cmds = Vec::new();
            if state == S::HoldUnderrun {
                cmds.push(C::AbandonUnderrunTimer);
            }
            (S::Stopped, cmds)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn start_and_resume_to_running() {
        let (next, cmds) = reduce(ClockState::Stopped, ClockEvent::Start);
        assert_eq!(next, ClockState::Running);
        assert!(cmds.is_empty());

        let (next, cmds) = reduce(ClockState::HoldScrub, ClockEvent::Resume);
        assert_eq!(next, ClockState::Running);
        assert!(cmds.is_empty());
    }

    #[test]
    fn resume_from_underrun_finishes_timer() {
        let (next, cmds) = reduce(ClockState::HoldUnderrun, ClockEvent::Resume);
        assert_eq!(next, ClockState::Running);
        assert_eq!(cmds, vec![ClockCmd::FinishUnderrunTimer]);
    }

    #[test]
    fn hold_underrun_notes_once() {
        let (next, cmds) = reduce(ClockState::Running, ClockEvent::HoldUnderrun);
        assert!(next.underrun_holding());
        assert_eq!(cmds, vec![ClockCmd::NoteUnderrunHoldStart]);

        let (next, cmds) = reduce(ClockState::HoldUnderrun, ClockEvent::HoldUnderrun);
        assert!(next.underrun_holding());
        assert!(cmds.is_empty());
    }

    #[test]
    fn scrub_hold_finishes_underrun() {
        let (next, cmds) = reduce(ClockState::HoldUnderrun, ClockEvent::HoldScrub);
        assert_eq!(next, ClockState::HoldScrub);
        assert!(next.is_holding());
        assert!(!next.underrun_holding());
        assert_eq!(cmds, vec![ClockCmd::FinishUnderrunTimer]);
    }

    #[test]
    fn underrun_and_scrub_block_bg_sync_verify() {
        let (underrun, _) = reduce(ClockState::Running, ClockEvent::HoldUnderrun);
        assert!(underrun.blocks_bg_sync_verify());

        let (scrub, _) = reduce(ClockState::Running, ClockEvent::HoldScrub);
        assert!(scrub.blocks_bg_sync_verify());

        assert!(!ClockState::Running.blocks_bg_sync_verify());
    }

    #[test]
    fn stop_from_underrun_finishes() {
        let (next, cmds) = reduce(ClockState::HoldUnderrun, ClockEvent::Stop);
        assert_eq!(next, ClockState::Stopped);
        assert_eq!(cmds, vec![ClockCmd::FinishUnderrunTimer]);
    }

    #[test]
    fn reset_for_play_abandons_timer() {
        let (next, cmds) = reduce(ClockState::HoldUnderrun, ClockEvent::ResetForPlay);
        assert_eq!(next, ClockState::Stopped);
        assert_eq!(cmds, vec![ClockCmd::AbandonUnderrunTimer]);
    }

    #[test]
    fn clear_session_anchors_leaves_catchup() {
        let mut m = ClockModel {
            phase: ClockState::HoldScrub,
            held_origin: Some(1.5),
            underrun_since_ms: Some(99.0),
            pending_silent: true,
            resuming: true,
            catchup_until_ms: Some(1000.0),
        };
        m.clear_session_anchors();
        assert_eq!(m.held_origin, None);
        assert_eq!(m.underrun_since_ms, None);
        assert!(!m.pending_silent);
        assert!(!m.resuming);
        assert_eq!(m.catchup_until_ms, Some(1000.0));
    }
}
