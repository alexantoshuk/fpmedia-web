//! Pure host OpQueue coalesce rules (CallSerializer / asyncId).
//!
//! Spec / unit tests for the retain+insert rules used by
//! `media_components::video::op_queue`. The live queue mutates `HostOp` in place
//! so each entry keeps its enqueue epoch — do **not** rematerialize pending ops
//! with the current `async_id` (that revives scrub left stale after Pause bump).
//!
//! mediabunny: `CallSerializer` + demo `asyncId` — latest seek/scrub wins.
//! Prefer [`apply_scrub_release`] (atomic settle±play) over settle+PlayPrime pairs
//! so drag cannot land between them (`architecture.md` § State machines over flags).

use std::collections::VecDeque;

/// Tag-only host op (no source / canvas) for coalesce unit tests and queue logic.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostOpKind {
    Open,
    Seek(u32),
    SeekKeyframe(u32),
    /// Drag only (`settle=false`). Prefer [`HostOpKind::ScrubRelease`] for pointer-up.
    Scrub { frame: u32, settle: bool },
    /// Atomic pointer-up: exact settle, then optional play-prime in the same op.
    ScrubRelease {
        frame: u32,
        then_play: bool,
        settle_gen: u64,
    },
    Step(bool),
    PlayPrime,
}

/// Pointer-up (and transport scrub-release): drop pending seek/step/scrub/play/
/// prior release; keep Open; append one [`HostOpKind::ScrubRelease`].
pub fn apply_scrub_release(
    pending: &mut VecDeque<HostOpKind>,
    frame: u32,
    then_play: bool,
    settle_gen: u64,
) {
    pending.retain(|o| matches!(o, HostOpKind::Open));
    pending.push_back(HostOpKind::ScrubRelease {
        frame,
        then_play, settle_gen,
    });
}

/// Apply scrub enqueue rules to a pending kind queue (mutates in place).
///
/// **Settle (`settle=true`):** legacy path — keep Open + PlayPrime; insert scrub
/// before PlayPrime. Prefer [`apply_scrub_release`] for new code.
/// **Drag:** keep Open only; append scrub (drops pending PlayPrime / ScrubRelease).
pub fn apply_scrub(pending: &mut VecDeque<HostOpKind>, frame: u32, settle: bool) {
    if settle {
        pending.retain(|o| {
            matches!(
                o,
                HostOpKind::Open | HostOpKind::PlayPrime | HostOpKind::ScrubRelease { .. }
            )
        });
        // Prefer collapsing into ScrubRelease when a release with then_play is pending.
        if let Some(pos) = pending.iter().position(|o| {
            matches!(
                o,
                HostOpKind::ScrubRelease {
                    then_play: true,
                    ..
                }
            )
        }) {
            let settle_gen = match pending[pos] {
                HostOpKind::ScrubRelease { settle_gen, .. } => settle_gen,
                _ => 0,
            };
            pending[pos] = HostOpKind::ScrubRelease {
                frame,
                then_play: true, settle_gen,
            };
            return;
        }
        let scrub = HostOpKind::Scrub {
            frame,
            settle: true,
        };
        if let Some(pos) = pending
            .iter()
            .position(|o| matches!(o, HostOpKind::PlayPrime))
        {
            pending.insert(pos, scrub);
        } else {
            pending.push_back(scrub);
        }
    } else {
        pending.retain(|o| matches!(o, HostOpKind::Open));
        pending.push_back(HostOpKind::Scrub {
            frame,
            settle: false,
        });
    }
}

/// Seek/step: keep Open only; append op.
pub fn apply_seek_or_step(pending: &mut VecDeque<HostOpKind>, op: HostOpKind) {
    debug_assert!(matches!(
        op,
        HostOpKind::Seek(_) | HostOpKind::SeekKeyframe(_) | HostOpKind::Step(_)
    ));
    pending.retain(|o| matches!(o, HostOpKind::Open));
    pending.push_back(op);
}

/// Play-prime: drop other PlayPrimes; append (keeps scrub settle / open /
/// ScrubRelease). Does not strip `ScrubRelease { then_play: true }` — that op
/// already owns play.
pub fn apply_play(pending: &mut VecDeque<HostOpKind>) {
    if pending
        .iter()
        .any(|o| matches!(o, HostOpKind::ScrubRelease { then_play: true, .. }))
    {
        return;
    }
    pending.retain(|o| !matches!(o, HostOpKind::PlayPrime));
    pending.push_back(HostOpKind::PlayPrime);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drag_scrub_drops_play_keeps_open() {
        let mut q = VecDeque::from([HostOpKind::Open, HostOpKind::PlayPrime, HostOpKind::Seek(3)]);
        apply_scrub(&mut q, 10, false);
        assert_eq!(
            q.iter().copied().collect::<Vec<_>>(),
            vec![
                HostOpKind::Open,
                HostOpKind::Scrub {
                    frame: 10,
                    settle: false
                }
            ]
        );
    }

    #[test]
    fn settle_inserts_before_play_prime() {
        let mut q = VecDeque::from([HostOpKind::Open, HostOpKind::PlayPrime]);
        apply_scrub(&mut q, 7, true);
        assert_eq!(
            q.iter().copied().collect::<Vec<_>>(),
            vec![
                HostOpKind::Open,
                HostOpKind::Scrub {
                    frame: 7,
                    settle: true
                },
                HostOpKind::PlayPrime
            ]
        );
    }

    #[test]
    fn scrub_release_replaces_pending_atomically() {
        let mut q = VecDeque::from([HostOpKind::Open, HostOpKind::PlayPrime]);
        apply_scrub_release(&mut q, 11, true, 1);
        assert_eq!(
            q.iter().copied().collect::<Vec<_>>(),
            vec![
                HostOpKind::Open,
                HostOpKind::ScrubRelease {
                    frame: 11,
                    then_play: true,
                    settle_gen: 1
                }
            ]
        );
        apply_scrub_release(&mut q, 22, true, 2);
        assert_eq!(
            q.iter().copied().collect::<Vec<_>>(),
            vec![
                HostOpKind::Open,
                HostOpKind::ScrubRelease {
                    frame: 22,
                    then_play: true,
                    settle_gen: 2
                }
            ]
        );
    }

    #[test]
    fn play_noop_when_scrub_release_owns_play() {
        let mut q = VecDeque::from([HostOpKind::ScrubRelease {
            frame: 4,
            then_play: true,
            settle_gen: 1,
        }]);
        apply_play(&mut q);
        assert_eq!(
            q.iter().copied().collect::<Vec<_>>(),
            vec![HostOpKind::ScrubRelease {
                frame: 4,
                then_play: true,
                settle_gen: 1
            }]
        );
    }

    #[test]
    fn settle_without_play_appends() {
        let mut q = VecDeque::from([HostOpKind::Open]);
        apply_scrub(&mut q, 2, true);
        assert_eq!(
            q.iter().copied().collect::<Vec<_>>(),
            vec![
                HostOpKind::Open,
                HostOpKind::Scrub {
                    frame: 2,
                    settle: true
                }
            ]
        );
    }

    #[test]
    fn seek_drops_scrub_and_play() {
        let mut q = VecDeque::from([
            HostOpKind::Open,
            HostOpKind::Scrub {
                frame: 1,
                settle: true,
            },
            HostOpKind::PlayPrime,
        ]);
        apply_seek_or_step(&mut q, HostOpKind::Seek(9));
        assert_eq!(
            q.iter().copied().collect::<Vec<_>>(),
            vec![HostOpKind::Open, HostOpKind::Seek(9)]
        );
    }

    #[test]
    fn play_replaces_pending_play_keeps_settle() {
        let mut q = VecDeque::from([
            HostOpKind::Open,
            HostOpKind::Scrub {
                frame: 4,
                settle: true,
            },
            HostOpKind::PlayPrime,
        ]);
        apply_play(&mut q);
        assert_eq!(
            q.iter().copied().collect::<Vec<_>>(),
            vec![
                HostOpKind::Open,
                HostOpKind::Scrub {
                    frame: 4,
                    settle: true
                },
                HostOpKind::PlayPrime
            ]
        );
    }

    /// Documented hazard: after `bump`, a pending scrub must stay on its old id
    /// so drain skips it — rematerializing with the new epoch revives it and can
    /// block Play (fMP4 scrub→Pause→Play / play-after-scrub).
    #[test]
    fn coalesce_does_not_assign_epochs() {
        // host_op_coalesce is kind-only; epoch preservation is OpQueue's job.
        let mut q = VecDeque::from([HostOpKind::Scrub {
            frame: 10,
            settle: true,
        }]);
        apply_play(&mut q);
        assert_eq!(q.len(), 2);
        assert!(matches!(q[0], HostOpKind::Scrub { settle: true, .. }));
        assert_eq!(q[1], HostOpKind::PlayPrime);
    }
}
