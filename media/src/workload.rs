//! Thin workload policy knobs (Play > Seek > Open > Bg).
//!
//! **Not** a global async priority OS — domain schedulers (OpQueue, decode,
//! byte IO) stay separate. This module only classifies whether background
//! sync-verify / fragment-backfill should yield.
//!
//! See `.ai/architecture.md` § State machines over flags / ADR domain schedulers.
//!
//! mediabunny: `verifyKeyPackets` on packet retrieve — no background full-file
//! sync walk during playback. Our deferred `verify_next_pending` loop is a local
//! adaptation for scrub geometry and must yield while Play / Seek own the network.

use std::fmt;

use crate::engine_mode::EngineMode;

/// Background-work priority band (highest first for *yielding* bg work).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorkloadMode {
    /// Presentation owns network / decode — bg verify yields.
    Play,
    /// Exact settle / seek / restarting — bg verify yields.
    Seek,
    /// Progressive open / warm. UI stages are [`crate::OpenPhase`]; this band is
    /// not classified yet (open still runs to completion on the host OpQueue).
    Open,
    /// Paused / scrub densify — bg verify may run when densify idle.
    Bg,
}

impl fmt::Display for WorkloadMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Play => write!(f, "Play"),
            Self::Seek => write!(f, "Seek"),
            Self::Open => write!(f, "Open"),
            Self::Bg => write!(f, "Bg"),
        }
    }
}

/// Snapshot inputs for [`classify`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WorkloadHints {
    pub engine: EngineMode,
    /// Seek / scrub settle async owns the main decoder.
    pub restarting: bool,
    /// Media clock frozen for underrun.
    pub underrun_holding: bool,
    /// Densify fill task running.
    pub scrub_busy: bool,
}

impl WorkloadHints {
    pub fn new(
        engine: EngineMode,
        restarting: bool,
        underrun_holding: bool,
        scrub_busy: bool,
    ) -> Self {
        Self {
            engine,
            restarting,
            underrun_holding,
            scrub_busy,
        }
    }
}

/// Map engine + transient seeks into a workload band.
pub fn classify(hints: WorkloadHints) -> WorkloadMode {
    match hints.engine {
        EngineMode::Play => WorkloadMode::Play,
        EngineMode::ExactHold => WorkloadMode::Seek,
        EngineMode::Scrub if hints.restarting => WorkloadMode::Seek,
        EngineMode::Scrub => WorkloadMode::Bg,
    }
}

/// Whether deferred bitstream sync-verify may issue a Range / candidate step.
///
/// False under Play / Seek, underrun hold, or densify busy (URL 502 / Range storms).
pub fn allows_bg_sync_verify(hints: WorkloadHints) -> bool {
    if hints.underrun_holding || hints.scrub_busy {
        return false;
    }
    matches!(classify(hints), WorkloadMode::Bg | WorkloadMode::Open)
}

/// Whether fMP4 fragment backfill should `yield_now` between chunks.
///
/// Keeps pumping when Play, urgent, or index lead `< min_ahead`; otherwise idle-yield
/// so scrub/pause can breathe.
pub fn fragment_backfill_should_idle_yield(
    mode: WorkloadMode,
    ahead: u64,
    urgent: bool,
    min_ahead: u64,
) -> bool {
    if urgent || matches!(mode, WorkloadMode::Play) || ahead < min_ahead {
        return false;
    }
    true
}

/// Default index lead before fragment backfill idle-yields (frames).
pub const FRAGMENT_BACKFILL_IDLE_AHEAD: u64 = 240;

#[cfg(test)]
mod tests {
    use super::*;

    fn hints(
        engine: EngineMode,
        restarting: bool,
        underrun: bool,
        scrub_busy: bool,
    ) -> WorkloadHints {
        WorkloadHints::new(engine, restarting, underrun, scrub_busy)
    }

    #[test]
    fn classify_play_seek_bg() {
        assert_eq!(
            classify(hints(EngineMode::Play, false, false, false)),
            WorkloadMode::Play
        );
        assert_eq!(
            classify(hints(EngineMode::ExactHold, false, false, false)),
            WorkloadMode::Seek
        );
        assert_eq!(
            classify(hints(EngineMode::Scrub, true, false, false)),
            WorkloadMode::Seek
        );
        assert_eq!(
            classify(hints(EngineMode::Scrub, false, false, false)),
            WorkloadMode::Bg
        );
    }

    #[test]
    fn verify_yields_on_play_and_seek() {
        assert!(!allows_bg_sync_verify(hints(
            EngineMode::Play,
            false,
            false,
            false
        )));
        assert!(!allows_bg_sync_verify(hints(
            EngineMode::ExactHold,
            false,
            false,
            false
        )));
        assert!(!allows_bg_sync_verify(hints(
            EngineMode::Scrub,
            true,
            false,
            false
        )));
    }

    #[test]
    fn verify_allowed_on_idle_scrub() {
        assert!(allows_bg_sync_verify(hints(
            EngineMode::Scrub,
            false,
            false,
            false
        )));
    }

    #[test]
    fn verify_yields_when_scrub_busy_or_underrun() {
        assert!(!allows_bg_sync_verify(hints(
            EngineMode::Scrub,
            false,
            false,
            true
        )));
        assert!(!allows_bg_sync_verify(hints(
            EngineMode::Scrub,
            false,
            true,
            false
        )));
    }

    #[test]
    fn backfill_idle_yield_table() {
        assert!(!fragment_backfill_should_idle_yield(
            WorkloadMode::Play,
            500,
            false,
            FRAGMENT_BACKFILL_IDLE_AHEAD
        ));
        assert!(!fragment_backfill_should_idle_yield(
            WorkloadMode::Bg,
            100,
            false,
            FRAGMENT_BACKFILL_IDLE_AHEAD
        ));
        assert!(!fragment_backfill_should_idle_yield(
            WorkloadMode::Bg,
            500,
            true,
            FRAGMENT_BACKFILL_IDLE_AHEAD
        ));
        assert!(fragment_backfill_should_idle_yield(
            WorkloadMode::Bg,
            500,
            false,
            FRAGMENT_BACKFILL_IDLE_AHEAD
        ));
        assert!(fragment_backfill_should_idle_yield(
            WorkloadMode::Seek,
            500,
            false,
            FRAGMENT_BACKFILL_IDLE_AHEAD
        ));
    }
}
