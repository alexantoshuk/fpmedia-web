//! Engine play vs scrub densify / soft-hold audio.
//!
//! Replaces `CacheFillMode` + `hold_densify_for_exact` + `audio_warm_resume` latches.
//! See `.ai/architecture.md` § State machines over flags.
//!
//! mediabunny: `examples/media-player/media-player.ts` `seekToTime` → `pause` then
//! `play` without recreating `AudioContext` (iterator `.return()` only). Path match:
//! Play = sequential sink (`canvases` / samples); Scrub + ExactHold = sparse access
//! (`getCanvas` / `canvasesAtTimestamps`); exact settle owns the decoder while densify
//! waits.

use std::fmt;

/// Play vs scrub densify vs exact-settle (HW owned by key→N).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EngineMode {
    /// Presentation queue; densify cancelled.
    Play,
    /// Paused / drag: gap-fill densify allowed (if [`crate::ScrubPolicy::densify`]).
    Scrub,
    /// Settle / sync seek owns main HW; densify deferred (`scrub_pending`).
    ExactHold,
}

impl fmt::Display for EngineMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Play => write!(f, "Play"),
            Self::Scrub => write!(f, "Scrub"),
            Self::ExactHold => write!(f, "ExactHold"),
        }
    }
}

/// Soft-held audio after Pause / scrub-while-play → warm `restart_warm` on PlayPrime.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AudioResume {
    /// Open / EOF / abort-prime — cold `start_media_clock`.
    /// Pause button soft-holds (`Warm`) like scrub.
    Cold,
    /// Soft-hold — next resume uses `restart_warm`.
    Warm,
}

/// Pure engine mode + audio resume latch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EngineModel {
    pub mode: EngineMode,
    pub audio: AudioResume,
}

impl Default for EngineModel {
    fn default() -> Self {
        Self {
            mode: EngineMode::Scrub,
            audio: AudioResume::Cold,
        }
    }
}

impl EngineModel {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_play(self) -> bool {
        matches!(self.mode, EngineMode::Play)
    }

    /// Densify fill may run (Scrub only — not Play, not ExactHold).
    pub fn allows_densify(self) -> bool {
        matches!(self.mode, EngineMode::Scrub)
    }

    pub fn holds_exact(self) -> bool {
        matches!(self.mode, EngineMode::ExactHold)
    }

    pub fn warm_audio(self) -> bool {
        matches!(self.audio, AudioResume::Warm)
    }

    /// Consume Warm → Cold. Returns whether warm resume was pending.
    pub fn take_warm_audio(&mut self) -> bool {
        if self.audio == AudioResume::Warm {
            self.audio = AudioResume::Cold;
            true
        } else {
            false
        }
    }
}

/// Input to [`reduce`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EngineEvent {
    EnterPlay,
    /// Leave play (or reinforce scrub). `soft_audio` → Warm + SoftHoldClock.
    /// While [`EngineMode::ExactHold`], mode stays ExactHold (densify stays deferred).
    EnterScrub {
        soft_audio: bool,
    },
    /// Settle / sync seek — densify waits.
    BeginExact,
    /// Clear exact hold → Scrub; densify may rearm.
    EndExact,
    /// Open / EOF / abort-prime — clear Warm.
    HardStopAudio,
}

/// Side effects for [`crate::VideoPlayer`] to apply.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EngineCmd {
    AbortDensify { rearm: bool },
    ScheduleDensify,
    SoftHoldClock,
    HardStopClock,
}

/// Pure transition.
pub fn reduce(model: EngineModel, event: EngineEvent) -> (EngineModel, Vec<EngineCmd>) {
    use AudioResume as A;
    use EngineCmd as C;
    use EngineEvent as E;
    use EngineMode as M;

    match event {
        E::EnterPlay => (
            EngineModel {
                mode: M::Play,
                audio: model.audio,
            },
            vec![C::AbortDensify { rearm: false }],
        ),

        E::EnterScrub { soft_audio } => {
            let mode = if model.mode == M::ExactHold {
                M::ExactHold
            } else {
                M::Scrub
            };
            let audio = if soft_audio { A::Warm } else { A::Cold };
            let mut cmds = Vec::new();
            if soft_audio {
                cmds.push(C::SoftHoldClock);
            } else {
                cmds.push(C::HardStopClock);
            }
            if mode == M::Scrub {
                cmds.push(C::ScheduleDensify);
            }
            (EngineModel { mode, audio }, cmds)
        }

        E::BeginExact => (
            EngineModel {
                mode: M::ExactHold,
                audio: model.audio,
            },
            vec![C::AbortDensify { rearm: true }],
        ),

        E::EndExact => {
            if model.mode != M::ExactHold {
                return (model, vec![]);
            }
            (
                EngineModel {
                    mode: M::Scrub,
                    audio: model.audio,
                },
                vec![C::ScheduleDensify],
            )
        }

        E::HardStopAudio => (
            EngineModel {
                mode: model.mode,
                audio: A::Cold,
            },
            vec![C::HardStopClock],
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enter_play_keeps_warm_aborts_densify() {
        let m = EngineModel {
            mode: EngineMode::ExactHold,
            audio: AudioResume::Warm,
        };
        let (next, cmds) = reduce(m, EngineEvent::EnterPlay);
        assert_eq!(next.mode, EngineMode::Play);
        assert_eq!(next.audio, AudioResume::Warm);
        assert!(next.is_play());
        assert!(!next.allows_densify());
        assert_eq!(cmds, vec![EngineCmd::AbortDensify { rearm: false }]);
    }

    #[test]
    fn scrub_soft_sets_warm() {
        let (next, cmds) = reduce(
            EngineModel::new(),
            EngineEvent::EnterScrub { soft_audio: true },
        );
        assert_eq!(next.mode, EngineMode::Scrub);
        assert!(next.warm_audio());
        assert!(next.allows_densify());
        assert_eq!(
            cmds,
            vec![EngineCmd::SoftHoldClock, EngineCmd::ScheduleDensify]
        );
    }

    #[test]
    fn scrub_while_exact_keeps_hold() {
        let m = EngineModel {
            mode: EngineMode::ExactHold,
            audio: AudioResume::Cold,
        };
        let (next, cmds) = reduce(m, EngineEvent::EnterScrub { soft_audio: true });
        assert!(next.holds_exact());
        assert!(next.warm_audio());
        assert!(!next.allows_densify());
        assert_eq!(cmds, vec![EngineCmd::SoftHoldClock]);
    }

    #[test]
    fn begin_exact_defers_densify() {
        let (next, cmds) = reduce(EngineModel::new(), EngineEvent::BeginExact);
        assert!(next.holds_exact());
        assert!(!next.allows_densify());
        assert_eq!(cmds, vec![EngineCmd::AbortDensify { rearm: true }]);
    }

    #[test]
    fn end_exact_to_scrub_schedules() {
        let m = EngineModel {
            mode: EngineMode::ExactHold,
            audio: AudioResume::Warm,
        };
        let (next, cmds) = reduce(m, EngineEvent::EndExact);
        assert_eq!(next.mode, EngineMode::Scrub);
        assert!(next.warm_audio());
        assert_eq!(cmds, vec![EngineCmd::ScheduleDensify]);
    }

    #[test]
    fn end_exact_noop_when_not_holding() {
        let m = EngineModel {
            mode: EngineMode::Scrub,
            audio: AudioResume::Cold,
        };
        let (next, cmds) = reduce(m, EngineEvent::EndExact);
        assert_eq!(next, m);
        assert!(cmds.is_empty());
    }

    #[test]
    fn hard_stop_clears_warm() {
        let m = EngineModel {
            mode: EngineMode::Scrub,
            audio: AudioResume::Warm,
        };
        let (next, cmds) = reduce(m, EngineEvent::HardStopAudio);
        assert!(!next.warm_audio());
        assert_eq!(cmds, vec![EngineCmd::HardStopClock]);
    }

    #[test]
    fn take_warm_audio_consumes() {
        let mut m = EngineModel {
            mode: EngineMode::Play,
            audio: AudioResume::Warm,
        };
        assert!(m.take_warm_audio());
        assert!(!m.warm_audio());
        assert!(!m.take_warm_audio());
    }
}
