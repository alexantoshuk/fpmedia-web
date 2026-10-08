//! Pure host OpQueue coalesce rules (CallSerializer / asyncId).
//!
//! Single entry: [`apply_enqueue`]. The live queue in `media_components` stores
//! `(epoch, HostOp)` entries and coalesces them in place: survivors keep their
//! enqueue epoch — never rematerialize them with the current `async_id` (that
//! revives work superseded by a bump, e.g. scrub after Pause, and can block Play).
//!
//! mediabunny: `CallSerializer` + demo `asyncId` — latest seek/scrub wins.
//! Canonical pointer-up: [`HostOp::ScrubRelease`] (atomic settle±play).
//! See `.ai/architecture.md` § State machines over flags.

use std::collections::VecDeque;

/// Host media op. `O` = open arguments (source + canvas in the host; `()` in tests).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostOp<O> {
    Open(O),
    /// Exact presentation frame.
    Seek(u32),
    /// Nearest sync at/before frame.
    SeekKeyframe(u32),
    /// Timeline drag — cancelable; latest target wins. Pointer-up uses
    /// [`HostOp::ScrubRelease`].
    ScrubDrag(u32),
    /// Atomic pointer-up: exact settle, then optional play-prime in the same op.
    ScrubRelease {
        frame: u32,
        then_play: bool,
        /// Transport settle generation — late Done must match.
        settle_gen: u64,
    },
    Step(bool),
    /// Prime presentation queue then start the play session.
    PlayPrime,
}

impl<O> HostOp<O> {
    pub fn is_open(&self) -> bool {
        matches!(self, Self::Open(_))
    }

    /// `ScrubRelease { then_play: true }` — owns the pending play request.
    pub fn is_release_with_play(&self) -> bool {
        matches!(
            self,
            Self::ScrubRelease {
                then_play: true,
                ..
            }
        )
    }

    /// PlayPrime or a release that resumes play.
    pub fn starts_play(&self) -> bool {
        matches!(self, Self::PlayPrime) || self.is_release_with_play()
    }
}

impl<O> AsRef<HostOp<O>> for HostOp<O> {
    fn as_ref(&self) -> &HostOp<O> {
        self
    }
}

/// Coalesce matrix: mutate `pending` for the `incoming` entry.
///
/// | Incoming | Keep | Drop / merge |
/// | -------- | ---- | ------------ |
/// | Open | — | everything; append Open |
/// | ScrubDrag / ScrubRelease / Seek / SeekKeyframe / Step | Open | everything else; append |
/// | PlayPrime | Open + transport ops; skip if `ScrubRelease{then_play}` | other PlayPrimes; append PlayPrime |
pub fn apply_enqueue<O, E: AsRef<HostOp<O>>>(pending: &mut VecDeque<E>, incoming: E) {
    match incoming.as_ref() {
        HostOp::Open(_) => pending.clear(),
        HostOp::PlayPrime => {
            if pending.iter().any(|e| e.as_ref().is_release_with_play()) {
                return;
            }
            pending.retain(|e| !matches!(e.as_ref(), HostOp::PlayPrime));
        }
        HostOp::Seek(_)
        | HostOp::SeekKeyframe(_)
        | HostOp::ScrubDrag(_)
        | HostOp::ScrubRelease { .. }
        | HostOp::Step(_) => pending.retain(|e| e.as_ref().is_open()),
    }
    pending.push_back(incoming);
}

#[cfg(test)]
mod tests {
    use super::*;

    type Op = HostOp<()>;

    fn release(frame: u32, then_play: bool, settle_gen: u64) -> Op {
        HostOp::ScrubRelease {
            frame,
            then_play,
            settle_gen,
        }
    }

    fn run(q: &[Op], incoming: Op) -> Vec<Op> {
        let mut q = VecDeque::from(q.to_vec());
        apply_enqueue(&mut q, incoming);
        q.into_iter().collect()
    }

    #[test]
    fn open_replaces_everything() {
        assert_eq!(
            run(
                &[HostOp::Open(()), HostOp::Seek(3), HostOp::PlayPrime],
                HostOp::Open(())
            ),
            vec![HostOp::Open(())]
        );
    }

    #[test]
    fn scrub_drag_drops_play_keeps_open() {
        assert_eq!(
            run(
                &[HostOp::Open(()), HostOp::PlayPrime, HostOp::Seek(3)],
                HostOp::ScrubDrag(10)
            ),
            vec![HostOp::Open(()), HostOp::ScrubDrag(10)]
        );
    }

    #[test]
    fn scrub_release_replaces_pending_atomically() {
        let q = run(&[HostOp::Open(()), HostOp::PlayPrime], release(11, true, 1));
        assert_eq!(q, vec![HostOp::Open(()), release(11, true, 1)]);
        assert_eq!(
            run(&q, release(22, true, 2)),
            vec![HostOp::Open(()), release(22, true, 2)]
        );
    }

    #[test]
    fn play_noop_when_scrub_release_owns_play() {
        assert_eq!(
            run(&[release(4, true, 1)], HostOp::PlayPrime),
            vec![release(4, true, 1)]
        );
    }

    #[test]
    fn play_appends_when_release_is_settle_only() {
        assert_eq!(
            run(&[HostOp::Open(()), release(4, false, 1)], HostOp::PlayPrime),
            vec![HostOp::Open(()), release(4, false, 1), HostOp::PlayPrime]
        );
    }

    #[test]
    fn play_dedupes_play_and_keeps_transport_ops() {
        assert_eq!(
            run(&[HostOp::PlayPrime, HostOp::Seek(2)], HostOp::PlayPrime),
            vec![HostOp::Seek(2), HostOp::PlayPrime]
        );
    }

    #[test]
    fn seek_drops_scrub_and_play() {
        assert_eq!(
            run(
                &[HostOp::Open(()), release(1, true, 1), HostOp::PlayPrime],
                HostOp::Seek(9)
            ),
            vec![HostOp::Open(()), HostOp::Seek(9)]
        );
    }

    #[test]
    fn step_and_seek_key_keep_only_open() {
        let q = run(&[HostOp::Open(()), HostOp::PlayPrime], HostOp::Step(true));
        assert_eq!(q, vec![HostOp::Open(()), HostOp::Step(true)]);
        assert_eq!(
            run(&q, HostOp::SeekKeyframe(3)),
            vec![HostOp::Open(()), HostOp::SeekKeyframe(3)]
        );
    }

    /// Survivors keep their enqueue epoch: after a bump, a stale pending op must
    /// stay stale so drain skips it.
    #[test]
    fn survivors_keep_epoch() {
        struct Entry(u64, Op);
        impl AsRef<Op> for Entry {
            fn as_ref(&self) -> &Op {
                &self.1
            }
        }
        let mut q = VecDeque::from([Entry(1, HostOp::Seek(5))]);
        apply_enqueue(&mut q, Entry(2, HostOp::PlayPrime));
        let got: Vec<(u64, Op)> = q.into_iter().map(|Entry(e, o)| (e, o)).collect();
        assert_eq!(got, vec![(1, HostOp::Seek(5)), (2, HostOp::PlayPrime)]);
    }
}
