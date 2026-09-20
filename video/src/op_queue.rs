//! Host command serializer for open / seek / play-prime.
//!
//! mediabunny: `CallSerializer` (misc.ts) chains mutable decode work; the demo
//! player uses `asyncId` + iterator `.return()` so superseded async work is a no-op.
//! Here: one FIFO queue + a single `async_id` epoch. Bumping the epoch aborts the
//! in-flight command after its next `.await`; coalescing drops stale pending seeks.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::rc::Rc;

use std::sync::Arc;

use leptos::prelude::*;
use web_sys::HtmlCanvasElement;
use webmedia::MediaSource;

/// Commands that must not overlap on the shared [`webmedia::WebPlayer`].
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
    /// `settle` = pointer up (exact-first densify hold); drag keeps densify on.
    Scrub {
        frame: u32,
        settle: bool,
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

    /// Async scrub: drop pending seek/step/scrub (keep open). Drag also drops
    /// pending PlayPrime. **Settle keeps PlayPrime** and inserts ahead of it so
    /// play-after-scrub stays behind exact settle (`VideoRef::play` contract).
    pub fn enqueue_scrub(&self, frame: u32, settle: bool) -> bool {
        let id = self.async_id.get();
        let mut st = self.inner.borrow_mut();
        if settle {
            st.pending
                .retain(|(_, o)| matches!(o, HostOp::Open { .. } | HostOp::PlayPrime));
            let scrub = (
                id,
                HostOp::Scrub {
                    frame,
                    settle: true,
                },
            );
            if let Some(pos) = st
                .pending
                .iter()
                .position(|(_, o)| matches!(o, HostOp::PlayPrime))
            {
                st.pending.insert(pos, scrub);
            } else {
                st.pending.push_back(scrub);
            }
        } else {
            st.pending.retain(|(_, o)| matches!(o, HostOp::Open { .. }));
            st.pending.push_back((
                id,
                HostOp::Scrub {
                    frame,
                    settle: false,
                },
            ));
        }
        self.begin_drain_locked(&mut st)
    }

    /// Seek/step: drop pending seek/step/scrub/play (keep open), enqueue latest.
    /// Does **not** bump the epoch — that would cancel an in-flight open
    /// (mediabunny seeks await the prior iterator; open uses asyncId++).
    /// In-flight scrub is preempted by the caller via `cancel_ops` when
    /// [`Self::scrub_inflight`] is set. Sync step/seek wait for each other.
    /// Returns `true` to start drain.
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
    /// Pending scrub settle is kept — Play waits for it (same playhead) instead of
    /// cancelling and restarting the key→N cascade (`VideoRef::play` used to seek).
    /// Returns `true` to start drain.
    pub fn enqueue_play(&self) -> bool {
        let id = self.async_id.get();
        let mut st = self.inner.borrow_mut();
        st.pending.retain(|(_, o)| !matches!(o, HostOp::PlayPrime));
        st.pending.push_back((id, HostOp::PlayPrime));
        self.busy.set(true);
        self.begin_drain_locked(&mut st)
    }

    /// True when a PlayPrime is waiting (settle should skip densify arm).
    pub fn has_pending_play(&self) -> bool {
        self.inner
            .borrow()
            .pending
            .iter()
            .any(|(_, o)| matches!(o, HostOp::PlayPrime))
    }

    /// Drop queued play-prime (pause). Caller bumps epoch separately.
    pub fn clear_pending_play(&self) {
        self.inner
            .borrow_mut()
            .pending
            .retain(|(_, o)| !matches!(o, HostOp::PlayPrime));
        self.sync_busy();
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
