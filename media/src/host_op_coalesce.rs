//! Pure host OpQueue coalesce rules (CallSerializer / asyncId).
//!
//! Single entry: [`apply_enqueue`]. Live queue in `media_components` maps
//! `(epoch, HostOp)` ↔ [`HostOpKind`], applies this matrix, then rebuilds pending
//! **keeping epochs** on survivors (Open / in-flight scrub release) — never
//! rematerialize survivors with the current `async_id` (that revives scrub after
//! Pause bump and can block Play).
//!
//! mediabunny: `CallSerializer` + demo `asyncId` — latest seek/scrub wins.
//! Canonical pointer-up: [`EnqueueKind::ScrubRelease`] (atomic settle±play).
//! See `.ai/architecture.md` § State machines over flags.

use std::collections::VecDeque;

/// Tag-only host op (no source / canvas) for coalesce unit tests and queue logic.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostOpKind {
    Open,
    Seek(u32),
    SeekKeyframe(u32),
    /// Drag only. Pointer-up uses [`HostOpKind::ScrubRelease`].
    ScrubDrag {
        frame: u32,
    },
    /// Atomic pointer-up: exact settle, then optional play-prime in the same op.
    ScrubRelease {
        frame: u32,
        then_play: bool,
        settle_gen: u64,
    },
    Step(bool),
    PlayPrime,
}

/// What the host wants to put on the queue (one row of the coalesce matrix).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EnqueueKind {
    /// Keep Open only; append drag scrub.
    ScrubDrag {
        frame: u32,
    },
    /// Keep Open only; append/replace release (atomic settle ± play).
    ScrubRelease {
        frame: u32,
        then_play: bool,
        settle_gen: u64,
    },
    Seek(u32),
    SeekKeyframe(u32),
    Step(bool),
    /// Append PlayPrime unless a `ScrubRelease { then_play: true }` already owns play.
    PlayPrime,
}

/// Coalesce matrix: mutate `pending` for the incoming [`EnqueueKind`].
///
/// | Incoming | Keep | Drop / merge |
/// | -------- | ---- | ------------ |
/// | ScrubDrag | Open | everything else; append drag |
/// | ScrubRelease | Open | everything else; append release |
/// | Seek / SeekKeyframe / Step | Open | everything else; append |
/// | PlayPrime | Open + transport ops; skip if `ScrubRelease{then_play}` | other PlayPrimes; append PlayPrime |
pub fn apply_enqueue(pending: &mut VecDeque<HostOpKind>, op: EnqueueKind) {
    match op {
        EnqueueKind::ScrubDrag { frame } => {
            pending.retain(|o| matches!(o, HostOpKind::Open));
            pending.push_back(HostOpKind::ScrubDrag { frame });
        }
        EnqueueKind::ScrubRelease {
            frame,
            then_play,
            settle_gen,
        } => {
            pending.retain(|o| matches!(o, HostOpKind::Open));
            pending.push_back(HostOpKind::ScrubRelease {
                frame,
                then_play,
                settle_gen,
            });
        }
        EnqueueKind::Seek(t) => {
            pending.retain(|o| matches!(o, HostOpKind::Open));
            pending.push_back(HostOpKind::Seek(t));
        }
        EnqueueKind::SeekKeyframe(t) => {
            pending.retain(|o| matches!(o, HostOpKind::Open));
            pending.push_back(HostOpKind::SeekKeyframe(t));
        }
        EnqueueKind::Step(forward) => {
            pending.retain(|o| matches!(o, HostOpKind::Open));
            pending.push_back(HostOpKind::Step(forward));
        }
        EnqueueKind::PlayPrime => {
            if pending.iter().any(|o| {
                matches!(
                    o,
                    HostOpKind::ScrubRelease {
                        then_play: true,
                        ..
                    }
                )
            }) {
                return;
            }
            pending.retain(|o| !matches!(o, HostOpKind::PlayPrime));
            pending.push_back(HostOpKind::PlayPrime);
        }
    }
}

/// Pointer-up / transport scrub-release.
pub fn apply_scrub_release(
    pending: &mut VecDeque<HostOpKind>,
    frame: u32,
    then_play: bool,
    settle_gen: u64,
) {
    apply_enqueue(
        pending,
        EnqueueKind::ScrubRelease {
            frame,
            then_play,
            settle_gen,
        },
    );
}

/// Scrub drag only.
pub fn apply_scrub_drag(pending: &mut VecDeque<HostOpKind>, frame: u32) {
    apply_enqueue(pending, EnqueueKind::ScrubDrag { frame });
}

/// Seek/step: keep Open only; append op.
pub fn apply_seek_or_step(pending: &mut VecDeque<HostOpKind>, op: HostOpKind) {
    let kind = match op {
        HostOpKind::Seek(t) => EnqueueKind::Seek(t),
        HostOpKind::SeekKeyframe(t) => EnqueueKind::SeekKeyframe(t),
        HostOpKind::Step(f) => EnqueueKind::Step(f),
        other => panic!("apply_seek_or_step: not a seek/step: {other:?}"),
    };
    apply_enqueue(pending, kind);
}

/// Play-prime coalesce.
pub fn apply_play(pending: &mut VecDeque<HostOpKind>) {
    apply_enqueue(pending, EnqueueKind::PlayPrime);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scrub_drag_drops_play_keeps_open() {
        let mut q = VecDeque::from([HostOpKind::Open, HostOpKind::PlayPrime, HostOpKind::Seek(3)]);
        apply_enqueue(&mut q, EnqueueKind::ScrubDrag { frame: 10 });
        assert_eq!(
            q.iter().copied().collect::<Vec<_>>(),
            vec![HostOpKind::Open, HostOpKind::ScrubDrag { frame: 10 }]
        );
    }

    #[test]
    fn scrub_release_replaces_pending_atomically() {
        let mut q = VecDeque::from([HostOpKind::Open, HostOpKind::PlayPrime]);
        apply_enqueue(
            &mut q,
            EnqueueKind::ScrubRelease {
                frame: 11,
                then_play: true,
                settle_gen: 1,
            },
        );
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
        apply_enqueue(
            &mut q,
            EnqueueKind::ScrubRelease {
                frame: 22,
                then_play: true,
                settle_gen: 2,
            },
        );
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
        apply_enqueue(&mut q, EnqueueKind::PlayPrime);
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
    fn play_appends_when_release_is_settle_only() {
        let mut q = VecDeque::from([
            HostOpKind::Open,
            HostOpKind::ScrubRelease {
                frame: 4,
                then_play: false,
                settle_gen: 1,
            },
        ]);
        apply_enqueue(&mut q, EnqueueKind::PlayPrime);
        assert_eq!(
            q.iter().copied().collect::<Vec<_>>(),
            vec![
                HostOpKind::Open,
                HostOpKind::ScrubRelease {
                    frame: 4,
                    then_play: false,
                    settle_gen: 1
                },
                HostOpKind::PlayPrime
            ]
        );
    }

    #[test]
    fn seek_drops_scrub_and_play() {
        let mut q = VecDeque::from([
            HostOpKind::Open,
            HostOpKind::ScrubRelease {
                frame: 1,
                then_play: true,
                settle_gen: 1,
            },
            HostOpKind::PlayPrime,
        ]);
        apply_enqueue(&mut q, EnqueueKind::Seek(9));
        assert_eq!(
            q.iter().copied().collect::<Vec<_>>(),
            vec![HostOpKind::Open, HostOpKind::Seek(9)]
        );
    }

    #[test]
    fn step_and_seek_key_keep_only_open() {
        let mut q = VecDeque::from([HostOpKind::Open, HostOpKind::PlayPrime]);
        apply_enqueue(&mut q, EnqueueKind::Step(true));
        assert_eq!(
            q.iter().copied().collect::<Vec<_>>(),
            vec![HostOpKind::Open, HostOpKind::Step(true)]
        );
        apply_enqueue(&mut q, EnqueueKind::SeekKeyframe(3));
        assert_eq!(
            q.iter().copied().collect::<Vec<_>>(),
            vec![HostOpKind::Open, HostOpKind::SeekKeyframe(3)]
        );
    }

    /// Documented hazard: after `bump`, a pending scrub must stay on its old id
    /// so drain skips it — rematerializing with the new epoch revives it.
    /// Kind-only coalesce does not assign epochs (OpQueue's job).
    #[test]
    fn coalesce_does_not_assign_epochs() {
        let mut q = VecDeque::from([HostOpKind::ScrubRelease {
            frame: 10,
            then_play: false,
            settle_gen: 1,
        }]);
        apply_enqueue(&mut q, EnqueueKind::PlayPrime);
        assert_eq!(q.len(), 2);
        assert!(matches!(
            q[0],
            HostOpKind::ScrubRelease {
                then_play: false,
                ..
            }
        ));
        assert_eq!(q[1], HostOpKind::PlayPrime);
    }

    #[test]
    fn wrappers_match_apply_enqueue() {
        let mut a = VecDeque::from([HostOpKind::Open, HostOpKind::PlayPrime]);
        let mut b = a.clone();
        apply_scrub_drag(&mut a, 5);
        apply_enqueue(&mut b, EnqueueKind::ScrubDrag { frame: 5 });
        assert_eq!(a, b);
    }
}
