//! Host command serializer for open / seek / play-prime.
//!
//! mediabunny: `CallSerializer` (misc.ts) chains mutable decode work; the demo
//! player uses `asyncId` + iterator `.return()` so superseded async work is a no-op.
//! Here: one FIFO queue + a single `async_id` epoch. Bumping the epoch aborts the
//! in-flight command after its next `.await`; coalescing drops stale pending seeks.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::rc::Rc;

use leptos::prelude::*;
use web_sys::HtmlCanvasElement;

/// Commands that must not overlap on the shared [`webmedia::WebPlayer`].
#[derive(Debug)]
pub(crate) enum HostOp {
    Open {
        blob: web_sys::Blob,
        canvas: HtmlCanvasElement,
    },
    Seek(u32),
    Step(bool),
    /// Prime presentation queue then start the play session / rAF.
    PlayPrime,
}

struct QueueState {
    pending: VecDeque<(u64, HostOp)>,
    /// Drain loop is scheduled / running.
    draining: bool,
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
    pub fn enqueue_open(&self, blob: web_sys::Blob, canvas: HtmlCanvasElement) -> bool {
        let id = self.bump();
        let mut st = self.inner.borrow_mut();
        st.pending.clear();
        st.pending.push_back((
            id,
            HostOp::Open {
                blob,
                canvas,
            },
        ));
        self.busy.set(true);
        self.begin_drain_locked(&mut st)
    }

    /// Seek/step: drop pending seek/step/play (keep open), enqueue latest.
    /// Does **not** bump the epoch — that would cancel an in-flight open
    /// (mediabunny seeks await the prior iterator; open uses asyncId++).
    /// In-flight seek/play abort via `WebPlayer::pause` / `cancel_ops` and the
    /// `playing` flag. Returns `true` to start drain.
    pub fn enqueue_seek_or_step(&self, op: HostOp) -> bool {
        debug_assert!(matches!(op, HostOp::Seek(_) | HostOp::Step(_)));
        let id = self.async_id.get();
        let mut st = self.inner.borrow_mut();
        st.pending
            .retain(|(_, o)| matches!(o, HostOp::Open { .. }));
        st.pending.push_back((id, op));
        self.busy.set(true);
        self.begin_drain_locked(&mut st)
    }

    /// Play-prime: replace a pending PlayPrime; do not cancel an in-flight open/seek.
    /// Returns `true` to start drain.
    pub fn enqueue_play(&self) -> bool {
        let id = self.async_id.get();
        let mut st = self.inner.borrow_mut();
        st.pending.retain(|(_, o)| !matches!(o, HostOp::PlayPrime));
        st.pending.push_back((id, HostOp::PlayPrime));
        self.busy.set(true);
        self.begin_drain_locked(&mut st)
    }

    /// Drop queued play-prime (pause). Caller bumps epoch separately.
    pub fn clear_pending_play(&self) {
        self.inner
            .borrow_mut()
            .pending
            .retain(|(_, o)| !matches!(o, HostOp::PlayPrime));
        self.sync_busy();
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
