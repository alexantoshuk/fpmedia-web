//! Host command serializer for open / seek / play-prime.
//!
//! mediabunny: `CallSerializer` (misc.ts) chains mutable decode work; the demo
//! player uses `asyncId` + iterator `.return()` so superseded async work is a no-op.
//! Here: one FIFO queue + a single `async_id` epoch. Bumping the epoch aborts the
//! in-flight command after its next `.await`; coalescing drops stale pending seeks.
//!
//! Coalesce *rules* are unit-tested in [`webmedia::host_op_coalesce`] (keep in sync).
//! This queue mutates `HostOp` in place so each entry keeps its enqueue `async_id`
//! — rematerializing with the current id would revive ops left stale after
//! [`Self::bump`] (e.g. pending scrub after Pause → blocks Play).

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::rc::Rc;

use std::sync::Arc;

use leptos::prelude::*;
use web_sys::HtmlCanvasElement;
use webmedia::MediaSource;

/// Commands that must not overlap on the shared [`webmedia::VideoPlayer`].
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
    /// Atomic pointer-up: exact settle, then optional play-prime in the same op
    /// (`architecture.md` § State machines — no drag between settle and play).
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

    /// Async scrub drag: drop pending seek/step/scrub/release/play (keep open).
    /// Rule mirror: [`webmedia::apply_scrub`] (`settle=false`).
    pub fn enqueue_scrub_drag(&self, frame: u32) -> bool {
        let id = self.async_id.get();
        let mut st = self.inner.borrow_mut();
        st.pending.retain(|(_, o)| matches!(o, HostOp::Open { .. }));
        st.pending.push_back((
            id,
            HostOp::Scrub {
                frame,
                settle: false,
            },
        ));
        self.begin_drain_locked(&mut st)
    }

    /// Atomic scrub release (settle ± play). Drops pending transport ops; keeps Open.
    /// Rule mirror: [`webmedia::apply_scrub_release`].
    pub fn enqueue_scrub_release(&self, frame: u32, then_play: bool, settle_gen: u64) -> bool {
        let id = self.async_id.get();
        let mut st = self.inner.borrow_mut();
        st.pending.retain(|(_, o)| matches!(o, HostOp::Open { .. }));
        st.pending.push_back((
            id,
            HostOp::ScrubRelease {
                frame,
                then_play, settle_gen,
            },
        ));
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

    /// Seek/step: drop pending seek/step/scrub/play (keep open), enqueue latest.
    /// Does **not** bump the epoch — that would cancel an in-flight open
    /// (mediabunny seeks await the prior iterator; open uses asyncId++).
    /// In-flight scrub is preempted by the caller via `cancel_ops` when
    /// [`Self::scrub_inflight`] is set. Sync step/seek wait for each other.
    /// Returns `true` to start drain.
    ///
    /// Rule mirror: [`webmedia::apply_seek_or_step`].
    pub fn enqueue_seek_or_step(&self, op: HostOp) -> bool {
        debug_assert!(matches!(
            op,
            HostOp::Seek(_) | HostOp::SeekKeyframe(_) | HostOp::Step(_)
        ));
        let id = self.async_id.get();
        let mut st = self.inner.borrow_mut();
        st.pending.retain(|(_, o)| matches!(o, HostOp::Open { .. }));
        st.pending.push_back((id, op));
        self.busy.set(true);
        self.begin_drain_locked(&mut st)
    }

    /// True when the drain loop is inside a scrub op (sync step should cancel it).
    pub fn scrub_inflight(&self) -> bool {
        self.inner.borrow().scrub_inflight
    }

    pub fn set_scrub_inflight(&self, on: bool) {
        self.inner.borrow_mut().scrub_inflight = on;
    }

    /// Play-prime: replace a pending PlayPrime; do not cancel an in-flight open/seek.
    /// Pending scrub settle / ScrubRelease is kept — Play waits (or no-ops when
    /// `ScrubRelease { then_play }` already owns play).
    /// Returns `true` to start drain.
    ///
    /// Rule mirror: [`webmedia::apply_play`]. Surviving scrub entries keep their
    /// enqueue id (must not be refreshed to the current epoch).
    pub fn enqueue_play(&self) -> bool {
        let id = self.async_id.get();
        let mut st = self.inner.borrow_mut();
        if st.pending.iter().any(|(_, o)| {
            matches!(
                o,
                HostOp::ScrubRelease {
                    then_play: true,
                    ..
                }
            )
        }) {
            return self.begin_drain_locked(&mut st);
        }
        st.pending.retain(|(_, o)| !matches!(o, HostOp::PlayPrime));
        st.pending.push_back((id, HostOp::PlayPrime));
        self.busy.set(true);
        self.begin_drain_locked(&mut st)
    }

    /// True when a PlayPrime or `ScrubRelease { then_play }` is waiting
    /// (settle should skip densify arm).
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

    /// Current invalidation epoch (`asyncId`).
    pub fn current_id(&self) -> u64 {
        self.async_id.get()
    }

    /// Pop the next command for the drain loop.
    pub fn pop_next(&self) -> Option<(u64, HostOp)> {
        self.inner.borrow_mut().pending.pop_front()
    }

    /// End the drain loop when the queue is empty. Returns `true` if drain ended.
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

    fn sync_busy(&self) {
        let st = self.inner.borrow();
        if !st.draining && st.pending.is_empty() {
            self.busy.set(false);
        }
    }
}
