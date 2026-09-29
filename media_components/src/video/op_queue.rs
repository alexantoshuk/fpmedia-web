//! Host command serializer for open / seek / play-prime.
//!
//! mediabunny: `CallSerializer` (misc.ts) chains mutable decode work; the demo
//! player uses `asyncId` + iterator `.return()` so superseded async work is a no-op.
//! Here: one FIFO queue + a single `async_id` epoch. Bumping the epoch aborts the
//! in-flight command after its next `.await`; coalescing drops stale pending seeks.
//!
//! Coalesce *rules*: [`webmedia::apply_enqueue`] (keep in sync). This queue maps
//! to [`webmedia::HostOpKind`], applies the matrix, then rebuilds pending while
//! **preserving enqueue epochs** on survivors (Open / same-`settle_gen` release).

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::rc::Rc;
use std::sync::Arc;

use leptos::prelude::*;
use web_sys::HtmlCanvasElement;
use webmedia::{EnqueueKind, HostOpKind, MediaSource, apply_enqueue};

/// Commands that must not overlap on the shared [`webmedia::VideoPlayer`].
#[derive(Clone)]
pub(crate) enum HostOp {
    Open {
        source: Arc<dyn MediaSource>,
        canvas: HtmlCanvasElement,
    },
    /// Exact presentation frame (sync — waits in the drain queue).
    Seek(u32),
    /// Nearest sync at/before frame.
    SeekKeyframe(u32),
    /// Timeline drag — cancelable; latest target wins (does not latch `busy`).
    /// Drag only (`settle=false`). Pointer-up uses [`HostOp::ScrubRelease`].
    Scrub {
        frame: u32,
        settle: bool,
    },
    /// Atomic pointer-up: exact settle, then optional play-prime in the same op.
    ScrubRelease {
        frame: u32,
        then_play: bool,
        /// Transport settle generation — late Done must match.
        settle_gen: u64,
    },
    Step(bool),
    /// Prime presentation queue then start the play session / rAF.
    PlayPrime,
}

impl HostOp {
    fn to_kind(&self) -> HostOpKind {
        match self {
            Self::Open { .. } => HostOpKind::Open,
            Self::Seek(t) => HostOpKind::Seek(*t),
            Self::SeekKeyframe(t) => HostOpKind::SeekKeyframe(*t),
            Self::Scrub { frame, .. } => HostOpKind::ScrubDrag { frame: *frame },
            Self::ScrubRelease {
                frame,
                then_play,
                settle_gen,
            } => HostOpKind::ScrubRelease {
                frame: *frame,
                then_play: *then_play,
                settle_gen: *settle_gen,
            },
            Self::Step(f) => HostOpKind::Step(*f),
            Self::PlayPrime => HostOpKind::PlayPrime,
        }
    }
}

struct QueueState {
    pending: VecDeque<(u64, HostOp)>,
    /// Drain loop is scheduled / running.
    draining: bool,
    /// In-flight op is a cancelable scrub (sync step may preempt it).
    scrub_inflight: bool,
}

/// Serializes host media ops and tracks the invalidation epoch.
#[derive(Clone)]
pub(crate) struct OpQueue {
    inner: Rc<RefCell<QueueState>>,
    /// mediabunny `asyncId` — bumped to discard in-flight / pending work.
    async_id: Rc<Cell<u64>>,
    busy: RwSignal<bool>,
}

impl OpQueue {
    pub fn new(busy: RwSignal<bool>) -> Self {
        Self {
            inner: Rc::new(RefCell::new(QueueState {
                pending: VecDeque::new(),
                draining: false,
                scrub_inflight: false,
            })),
            async_id: Rc::new(Cell::new(0)),
            busy,
        }
    }

    /// Bump epoch so in-flight work bails after the next await.
    pub fn bump(&self) -> u64 {
        let next = self.async_id.get().wrapping_add(1);
        self.async_id.set(next);
        next
    }

    pub fn is_current(&self, id: u64) -> bool {
        self.async_id.get() == id
    }

    /// Open always supersedes: bump, drop pending, enqueue.
    /// Returns `true` if the caller should start the drain loop.
    pub fn enqueue_open(&self, source: Arc<dyn MediaSource>, canvas: HtmlCanvasElement) -> bool {
        let id = self.bump();
        let mut st = self.inner.borrow_mut();
        st.pending.clear();
        st.pending.push_back((id, HostOp::Open { source, canvas }));
        self.busy.set(true);
        self.begin_drain_locked(&mut st)
    }

    /// Async scrub drag. Rule: [`EnqueueKind::ScrubDrag`].
    pub fn enqueue_scrub_drag(&self, frame: u32) -> bool {
        let id = self.async_id.get();
        let mut st = self.inner.borrow_mut();
        coalesce_pending(&mut st.pending, id, EnqueueKind::ScrubDrag { frame });
        self.begin_drain_locked(&mut st)
    }

    /// Atomic scrub release (settle ± play). Rule: [`EnqueueKind::ScrubRelease`].
    pub fn enqueue_scrub_release(&self, frame: u32, then_play: bool, settle_gen: u64) -> bool {
        let id = self.async_id.get();
        let mut st = self.inner.borrow_mut();
        coalesce_pending(
            &mut st.pending,
            id,
            EnqueueKind::ScrubRelease {
                frame,
                then_play,
                settle_gen,
            },
        );
        self.busy.set(true);
        self.begin_drain_locked(&mut st)
    }

    /// Drop pending scrub / scrub-release / play (PreemptScrub). Keeps Open.
    pub fn clear_pending_scrub_and_play(&self) {
        self.inner.borrow_mut().pending.retain(|(_, o)| {
            !matches!(
                o,
                HostOp::Scrub { .. } | HostOp::ScrubRelease { .. } | HostOp::PlayPrime
            )
        });
        self.sync_busy();
    }

    /// Seek/step. Does **not** bump the epoch (open may still be in flight).
    pub fn enqueue_seek_or_step(&self, op: HostOp) -> bool {
        let kind = match &op {
            HostOp::Seek(t) => EnqueueKind::Seek(*t),
            HostOp::SeekKeyframe(t) => EnqueueKind::SeekKeyframe(*t),
            HostOp::Step(f) => EnqueueKind::Step(*f),
            _ => panic!("enqueue_seek_or_step: expected Seek/SeekKeyframe/Step"),
        };
        let id = self.async_id.get();
        let mut st = self.inner.borrow_mut();
        coalesce_pending(&mut st.pending, id, kind);
        self.busy.set(true);
        self.begin_drain_locked(&mut st)
    }

    pub fn scrub_inflight(&self) -> bool {
        self.inner.borrow().scrub_inflight
    }

    pub fn set_scrub_inflight(&self, on: bool) {
        self.inner.borrow_mut().scrub_inflight = on;
    }

    /// Play-prime. Rule: [`EnqueueKind::PlayPrime`].
    pub fn enqueue_play(&self) -> bool {
        let id = self.async_id.get();
        let mut st = self.inner.borrow_mut();
        coalesce_pending(&mut st.pending, id, EnqueueKind::PlayPrime);
        self.busy.set(true);
        self.begin_drain_locked(&mut st)
    }

    /// True when a PlayPrime or `ScrubRelease { then_play }` is waiting.
    pub fn has_pending_play(&self) -> bool {
        self.inner.borrow().pending.iter().any(|(_, o)| {
            matches!(o, HostOp::PlayPrime)
                || matches!(
                    o,
                    HostOp::ScrubRelease {
                        then_play: true,
                        ..
                    }
                )
        })
    }

    /// Abort in-flight work and drop every pending op (close / unload).
    pub fn cancel_all(&self) {
        self.bump();
        let mut st = self.inner.borrow_mut();
        st.pending.clear();
        st.scrub_inflight = false;
        if !st.draining {
            self.busy.set(false);
        }
    }

    pub fn current_id(&self) -> u64 {
        self.async_id.get()
    }

    pub fn pop_next(&self) -> Option<(u64, HostOp)> {
        self.inner.borrow_mut().pending.pop_front()
    }

    pub fn try_end_drain(&self) -> bool {
        let mut st = self.inner.borrow_mut();
        if !st.pending.is_empty() {
            return false;
        }
        st.draining = false;
        self.busy.set(false);
        true
    }

    fn begin_drain_locked(&self, st: &mut QueueState) -> bool {
        if st.draining {
            return false;
        }
        st.draining = true;
        true
    }

    /// Clear `busy` when nothing is draining / pending (superseded URL fetch).
    pub fn sync_busy(&self) {
        let st = self.inner.borrow();
        if !st.draining && st.pending.is_empty() {
            self.busy.set(false);
        }
    }
}

/// Apply [`apply_enqueue`] while preserving epochs on Open and same-`settle_gen`
/// ScrubRelease survivors.
fn coalesce_pending(pending: &mut VecDeque<(u64, HostOp)>, id: u64, kind: EnqueueKind) {
    let snapshot: Vec<(u64, HostOp)> = pending.iter().cloned().collect();
    let open = snapshot
        .iter()
        .find(|(_, o)| matches!(o, HostOp::Open { .. }))
        .cloned();
    let mut kinds: VecDeque<HostOpKind> = snapshot.iter().map(|(_, o)| o.to_kind()).collect();
    apply_enqueue(&mut kinds, kind);
    pending.clear();
    for k in kinds {
        match k {
            HostOpKind::Open => {
                if let Some(entry) = open.clone() {
                    pending.push_back(entry);
                }
            }
            HostOpKind::ScrubDrag { frame } => {
                pending.push_back((
                    id,
                    HostOp::Scrub {
                        frame,
                        settle: false,
                    },
                ));
            }
            HostOpKind::ScrubRelease {
                frame,
                then_play,
                settle_gen,
            } => {
                let eid = snapshot
                    .iter()
                    .find_map(|(e, o)| match o {
                        HostOp::ScrubRelease {
                            settle_gen: g,
                            then_play: tp,
                            ..
                        } if *g == settle_gen && *tp == then_play => Some(*e),
                        _ => None,
                    })
                    .unwrap_or(id);
                pending.push_back((
                    eid,
                    HostOp::ScrubRelease {
                        frame,
                        then_play,
                        settle_gen,
                    },
                ));
            }
            HostOpKind::Seek(t) => pending.push_back((id, HostOp::Seek(t))),
            HostOpKind::SeekKeyframe(t) => pending.push_back((id, HostOp::SeekKeyframe(t))),
            HostOpKind::Step(f) => pending.push_back((id, HostOp::Step(f))),
            HostOpKind::PlayPrime => pending.push_back((id, HostOp::PlayPrime)),
        }
    }
}
