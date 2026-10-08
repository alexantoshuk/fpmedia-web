//! Host playback transport state machine.
//!
//! Prefer this over sticky bools / TLS / Effects for play↔scrub races.
//! See `.ai/architecture.md` § State machines over flags.
//!
//! UI dispatches [`TransportEvent`]; a single [`reduce`] returns the next
//! [`TransportModel`] and [`TransportCmd`]s. `OpQueue` executes cmds — especially
//! atomic [`TransportCmd::EnqueueScrubRelease`] so drag cannot land between
//! settle and play-prime.
//!
//! Each scrub-release gets a **generation**; late `ScrubReleaseDone` from a
//! preempted release is ignored so transport cannot stick in `Playing` while
//! the UI is already soft-paused (multi-tap “stays paused until next click”).

use std::fmt;

/// Playback / scrub gesture phase (host layer).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransportState {
    Closed,
    Ready,
    Playing,
    Scrubbing {
        resume: bool,
        frame: u32,
    },
    Settling {
        resume: bool,
        frame: u32,
        /// Matches [`TransportCmd::EnqueueScrubRelease::settle_gen`] / Done.
        settle_gen: u64,
    },
    PlayPriming {
        from_scrub: bool,
    },
}

impl fmt::Display for TransportState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Closed => write!(f, "Closed"),
            Self::Ready => write!(f, "Ready"),
            Self::Playing => write!(f, "Playing"),
            Self::Scrubbing { resume, frame } => {
                write!(f, "Scrubbing(resume={resume}, frame={frame})")
            }
            Self::Settling {
                resume,
                frame,
                settle_gen,
            } => write!(
                f,
                "Settling(resume={resume}, frame={frame}, settle_gen={settle_gen})"
            ),
            Self::PlayPriming { from_scrub } => {
                write!(f, "PlayPriming(from_scrub={from_scrub})")
            }
        }
    }
}

/// FSM + settle-generation counter (pure; host owns the instance).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TransportModel {
    pub state: TransportState,
    /// Monotonic id assigned to each [`TransportCmd::EnqueueScrubRelease`].
    pub next_settle_gen: u64,
}

impl Default for TransportModel {
    fn default() -> Self {
        Self {
            state: TransportState::Closed,
            next_settle_gen: 1,
        }
    }
}

impl TransportModel {
    pub fn new() -> Self {
        Self::default()
    }
}

/// Input to [`reduce`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransportEvent {
    OpenReady,
    Closed,
    PlayPressed,
    PausePressed,
    PointerDown {
        frame: u32,
    },
    PointerMove {
        frame: u32,
    },
    PointerUp {
        frame: u32,
    },
    /// `ScrubRelease` op finished. `settle_gen` must match current [`TransportState::Settling`].
    ScrubReleaseDone {
        settle_gen: u64,
        then_play: bool,
        ok: bool,
    },
    PlayPrimeDone,
    PlayPrimeFailed,
    Ended,
}

/// Side effects for the host to apply (queue / engine / playhead).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransportCmd {
    SetPlayhead {
        frame: u32,
    },
    /// Leave play without wiping AudioDecoder (Pause button / scrub-while-play).
    ///
    /// `align_playhead`: snap UI playhead to the committed engine frame (Pause
    /// button). Scrub-while-play keeps `false` so `SetPlayhead` aim is not
    /// overwritten — otherwise PlayPrime `seek_frame`s the ~100 ms-stale UI
    /// playhead and redoes Exact before warm resume.
    SoftPause {
        align_playhead: bool,
    },
    /// Open / EOF / abort-prime / leave-scrub — bump epoch, cold audio next Play.
    HardPause,
    /// Cancel in-flight scrub/prime; **bump** queue epoch so late Done is dropped.
    PreemptScrub,
    EnqueueDrag {
        frame: u32,
    },
    /// Atomic settle (+ optional play). Drag cannot insert between the two.
    EnqueueScrubRelease {
        frame: u32,
        then_play: bool,
        settle_gen: u64,
    },
    EnqueuePlayPrime,
    ClearEnded,
}

/// Pure transition.
pub fn reduce(model: TransportModel, event: TransportEvent) -> (TransportModel, Vec<TransportCmd>) {
    use TransportCmd as C;
    use TransportEvent as E;
    use TransportState as S;

    let TransportModel {
        state,
        mut next_settle_gen,
    } = model;

    let (state, cmds) = match (state, event) {
        (_, E::Closed) => (S::Closed, vec![]),
        (S::Closed, E::OpenReady) => (S::Ready, vec![]),
        (S::Closed, _) => (S::Closed, vec![]),

        (_, E::OpenReady) => (S::Ready, vec![C::HardPause]),

        (S::Ready, E::PlayPressed) => (
            S::PlayPriming { from_scrub: false },
            vec![C::ClearEnded, C::EnqueuePlayPrime],
        ),
        // Soft-hold like mediabunny pause — keep AudioDecoder warm for instant Play.
        // Hard tear-down stays for open / EOF / abort-prime / leave-scrub.
        // Align playhead so PlayPrime does not seek on the throttled UI playhead.
        // mediabunny: examples/media-player/media-player.ts `pause` / `play`.
        (S::Playing, E::PausePressed) => (
            S::Ready,
            vec![C::SoftPause {
                align_playhead: true,
            }],
        ),
        (S::PlayPriming { .. }, E::PausePressed) => (S::Ready, vec![C::HardPause]),
        (S::Scrubbing { .. } | S::Settling { .. }, E::PausePressed) => {
            (S::Ready, vec![C::HardPause])
        }

        (S::Playing, E::PointerDown { frame }) => (
            S::Scrubbing {
                resume: true,
                frame,
            },
            vec![
                C::ClearEnded,
                C::SoftPause {
                    align_playhead: false,
                },
                C::SetPlayhead { frame },
                // Present soft-blit while finger moves (same as paused scrub).
                C::EnqueueDrag { frame },
            ],
        ),
        (S::Ready, E::PointerDown { frame }) => (
            S::Scrubbing {
                resume: false,
                frame,
            },
            vec![
                C::ClearEnded,
                C::SetPlayhead { frame },
                C::EnqueueDrag { frame },
            ],
        ),
        (
            S::Settling {
                resume,
                frame: _,
                settle_gen: _,
            },
            E::PointerDown { frame },
        ) => (
            S::Scrubbing { resume, frame },
            vec![
                C::PreemptScrub,
                C::SetPlayhead { frame },
                C::EnqueueDrag { frame },
            ],
        ),
        (S::PlayPriming { .. }, E::PointerDown { frame }) => (
            S::Scrubbing {
                resume: true,
                frame,
            },
            vec![
                C::HardPause,
                C::SetPlayhead { frame },
                C::EnqueueDrag { frame },
            ],
        ),
        (S::Scrubbing { resume, .. }, E::PointerDown { frame }) => (
            S::Scrubbing { resume, frame },
            vec![C::SetPlayhead { frame }, C::EnqueueDrag { frame }],
        ),

        (S::Scrubbing { resume, .. }, E::PointerMove { frame }) => (
            S::Scrubbing { resume, frame },
            vec![C::SetPlayhead { frame }, C::EnqueueDrag { frame }],
        ),

        (S::Scrubbing { resume, .. }, E::PointerUp { frame }) => {
            let settle_gen = next_settle_gen;
            next_settle_gen = next_settle_gen.wrapping_add(1);
            (
                S::Settling {
                    resume,
                    frame,
                    settle_gen,
                },
                vec![C::EnqueueScrubRelease {
                    frame,
                    then_play: resume,
                    settle_gen,
                }],
            )
        }

        (
            S::Settling {
                settle_gen: settling_gen,
                ..
            },
            E::ScrubReleaseDone {
                settle_gen,
                then_play: true,
                ok: true,
            },
        ) if settle_gen == settling_gen => (S::Playing, vec![]),
        (
            S::Settling {
                settle_gen: settling_gen,
                ..
            },
            E::ScrubReleaseDone {
                settle_gen,
                then_play: false,
                ok: true,
            },
        ) if settle_gen == settling_gen => (S::Ready, vec![]),
        (
            S::Settling {
                settle_gen: settling_gen,
                ..
            },
            E::ScrubReleaseDone {
                settle_gen,
                ok: false,
                ..
            },
        ) if settle_gen == settling_gen => (S::Ready, vec![]),

        // Stale / preempted release — ignore (keeps Scrubbing/Settling/Playing).
        (_, E::ScrubReleaseDone { .. }) => (state, vec![]),

        (S::PlayPriming { .. }, E::PlayPrimeDone) => (S::Playing, vec![]),
        (S::PlayPriming { .. }, E::PlayPrimeFailed) => (S::Ready, vec![]),

        (S::Playing, E::Ended) => (S::Ready, vec![]),
        (S::PlayPriming { .. }, E::Ended) => (S::Ready, vec![]),

        // Scrubbing with resume: Play arrives after pointer-up via then_play —
        // ignore while finger is down. Settling must always accept Play so a
        // cancelled/stuck release cannot leave transport deaf (Play no-op /
        // busy forever after cold scrub).
        (S::Scrubbing { resume: true, .. }, E::PlayPressed) => (state, vec![]),
        (S::Settling { .. } | S::Scrubbing { resume: false, .. }, E::PlayPressed) => (
            S::PlayPriming { from_scrub: false },
            vec![C::PreemptScrub, C::ClearEnded, C::EnqueuePlayPrime],
        ),
        // Failed/stuck prime used to stay in PlayPriming with no PlayPressed
        // arm (catch-all no-op) after stop_playback bumped away PlayPrimeFailed.
        (S::PlayPriming { .. }, E::PlayPressed) => (
            S::PlayPriming { from_scrub: false },
            vec![C::HardPause, C::ClearEnded, C::EnqueuePlayPrime],
        ),
        (S::Playing, E::PlayPressed) => (state, vec![]),

        (other, _) => (other, vec![]),
    };

    (
        TransportModel {
            state,
            next_settle_gen,
        },
        cmds,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn step(model: TransportModel, event: TransportEvent) -> (TransportModel, Vec<TransportCmd>) {
        reduce(model, event)
    }

    fn playing() -> TransportModel {
        TransportModel {
            state: TransportState::Playing,
            next_settle_gen: 1,
        }
    }

    #[test]
    fn play_from_ready_primes() {
        let m = TransportModel {
            state: TransportState::Ready,
            next_settle_gen: 1,
        };
        let (m, c) = step(m, TransportEvent::PlayPressed);
        assert_eq!(m.state, TransportState::PlayPriming { from_scrub: false });
        assert!(c.contains(&TransportCmd::EnqueuePlayPrime));
    }

    #[test]
    fn play_scrub_release_is_atomic() {
        let (m, c) = step(playing(), TransportEvent::PointerDown { frame: 10 });
        assert!(matches!(
            m.state,
            TransportState::Scrubbing {
                resume: true,
                frame: 10
            }
        ));
        assert!(c.contains(&TransportCmd::SoftPause {
            align_playhead: false
        }));
        assert!(c.contains(&TransportCmd::EnqueueDrag { frame: 10 }));

        let (m, c) = step(m, TransportEvent::PointerMove { frame: 12 });
        assert!(c.contains(&TransportCmd::EnqueueDrag { frame: 12 }));

        let (m, c) = step(m, TransportEvent::PointerUp { frame: 12 });
        assert_eq!(
            m.state,
            TransportState::Settling {
                resume: true,
                frame: 12,
                settle_gen: 1
            }
        );
        assert_eq!(
            c,
            vec![TransportCmd::EnqueueScrubRelease {
                frame: 12,
                then_play: true,
                settle_gen: 1
            }]
        );
        assert_eq!(m.next_settle_gen, 2);
    }

    #[test]
    fn stale_release_done_ignored_after_retap() {
        let settling = TransportModel {
            state: TransportState::Settling {
                resume: true,
                frame: 5,
                settle_gen: 1,
            },
            next_settle_gen: 2,
        };
        let (m, c) = step(settling, TransportEvent::PointerDown { frame: 20 });
        assert!(c.contains(&TransportCmd::PreemptScrub));
        let (m, _) = step(m, TransportEvent::PointerUp { frame: 20 });
        assert_eq!(
            m.state,
            TransportState::Settling {
                resume: true,
                frame: 20,
                settle_gen: 2
            }
        );
        // Late done from settle_gen=1 must not flip to Playing.
        let (m, _) = step(
            m,
            TransportEvent::ScrubReleaseDone {
                settle_gen: 1,
                then_play: true,
                ok: true,
            },
        );
        assert!(matches!(
            m.state,
            TransportState::Settling { settle_gen: 2, .. }
        ));
        let (m, _) = step(
            m,
            TransportEvent::ScrubReleaseDone {
                settle_gen: 2,
                then_play: true,
                ok: true,
            },
        );
        assert_eq!(m.state, TransportState::Playing);
    }

    #[test]
    fn stale_done_while_scrubbing_ignored() {
        let m = TransportModel {
            state: TransportState::Scrubbing {
                resume: true,
                frame: 3,
            },
            next_settle_gen: 2,
        };
        let (m, _) = step(
            m,
            TransportEvent::ScrubReleaseDone {
                settle_gen: 1,
                then_play: true,
                ok: true,
            },
        );
        assert!(matches!(m.state, TransportState::Scrubbing { .. }));
    }

    #[test]
    fn pause_scrub_drag_still_enqueues() {
        let m = TransportModel {
            state: TransportState::Ready,
            next_settle_gen: 1,
        };
        let (m, c) = step(m, TransportEvent::PointerDown { frame: 3 });
        assert!(c.contains(&TransportCmd::EnqueueDrag { frame: 3 }));
        let (m, c) = step(m, TransportEvent::PointerUp { frame: 3 });
        assert_eq!(
            c,
            vec![TransportCmd::EnqueueScrubRelease {
                frame: 3,
                then_play: false,
                settle_gen: 1
            }]
        );
        assert!(matches!(
            m.state,
            TransportState::Settling { resume: false, .. }
        ));
    }

    #[test]
    fn pause_while_playing_soft_holds_audio() {
        let m = TransportModel {
            state: TransportState::Playing,
            next_settle_gen: 1,
        };
        let (m, c) = step(m, TransportEvent::PausePressed);
        assert_eq!(m.state, TransportState::Ready);
        assert!(c.contains(&TransportCmd::SoftPause {
            align_playhead: true
        }));
        assert!(!c.contains(&TransportCmd::HardPause));
    }

    #[test]
    fn pause_clears_scrub_resume() {
        let m = TransportModel {
            state: TransportState::Scrubbing {
                resume: true,
                frame: 1,
            },
            next_settle_gen: 1,
        };
        let (m, c) = step(m, TransportEvent::PausePressed);
        assert_eq!(m.state, TransportState::Ready);
        assert!(c.contains(&TransportCmd::HardPause));
    }

    #[test]
    fn play_from_settling_preempts_and_primes() {
        // Stuck/cancelled Settling must not ignore Play (cold scrub recovery).
        let m = TransportModel {
            state: TransportState::Settling {
                resume: true,
                frame: 40,
                settle_gen: 3,
            },
            next_settle_gen: 4,
        };
        let (m, c) = step(m, TransportEvent::PlayPressed);
        assert_eq!(m.state, TransportState::PlayPriming { from_scrub: false });
        assert!(c.contains(&TransportCmd::PreemptScrub));
        assert!(c.contains(&TransportCmd::EnqueuePlayPrime));
    }

    #[test]
    fn play_from_play_priming_retries() {
        // Missed PlayPrimeFailed left transport in PlayPriming — Play must retry.
        let m = TransportModel {
            state: TransportState::PlayPriming { from_scrub: false },
            next_settle_gen: 1,
        };
        let (m, c) = step(m, TransportEvent::PlayPressed);
        assert_eq!(m.state, TransportState::PlayPriming { from_scrub: false });
        assert!(c.contains(&TransportCmd::HardPause));
        assert!(c.contains(&TransportCmd::EnqueuePlayPrime));
    }

    #[test]
    fn play_ignored_while_resume_scrubbing() {
        let m = TransportModel {
            state: TransportState::Scrubbing {
                resume: true,
                frame: 2,
            },
            next_settle_gen: 1,
        };
        let (m, c) = step(m, TransportEvent::PlayPressed);
        assert!(matches!(
            m.state,
            TransportState::Scrubbing { resume: true, .. }
        ));
        assert!(c.is_empty());
    }
}
