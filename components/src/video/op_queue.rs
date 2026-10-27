//! Host command serializer for open / seek / play-prime.
//!
//! mediabunny: `CallSerializer` (misc.ts) chains mutable decode work; the demo
//! player uses `asyncId` + iterator `.return()` so superseded async work is a no-op.
//! Here: one FIFO queue + a single `async_id` epoch. Bumping the epoch aborts the
//! in-flight command after its next `.await`; coalescing drops stale pending seeks.
//!
//! Coalesce rules: [`fpmedia_web::apply_enqueue`], applied in place so survivors keep
//! their enqueue epoch. A replacement release with the same `settle_gen` inherits
//! the epoch of the release it replaces.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::rc::Rc;
use std::sync::Arc;

use fpmedia_web::{MediaSource, apply_enqueue};
use leptos::prelude::*;
use web_sys::HtmlCanvasElement;

#[derive(Clone)]
pub(crate) struct OpenArgs {
    pub source: Arc<dyn MediaSource>,
    pub canvas: HtmlCanvasElement,
}

/// Commands that must not overlap on the shared [`fpmedia_web::VideoPlayer`].
pub(crate) type HostOp = fpmedia_web::HostOp<OpenArgs>;

struct Pending {
    epoch: u64,
    op: HostOp,
}

impl AsRef<HostOp> for Pending {
    fn as_ref(&self) -> &HostOp {
        &self.op
    }
}

struct QueueState {
    pending: VecDeque<Pending>,
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

    /// Coalesce `op` into pending ([`apply_enqueue`]). Open bumps the epoch
    /// (supersedes in-flight work); seek / step do not (open may still be in
    /// flight). Scrub drag does not latch `busy`.
    /// Returns `true` if the caller should start the drain loop.
    pub fn enqueue(&self, op: HostOp) -> bool {
        let id = if op.is_open() {
            self.bump()
        } else {
            self.async_id.get()
        };
        let mut st = self.inner.borrow_mut();
        let epoch = match &op {
            HostOp::ScrubRelease {
                then_play,
                settle_gen,
                ..
            } => st
                .pending
                .iter()
                .find_map(|p| match p.op {
                    HostOp::ScrubRelease {
                        settle_gen: g,
                        then_play: tp,
                        ..
                    } if g == *settle_gen && tp == *then_play => Some(p.epoch),
                    _ => None,
                })
                .unwrap_or(id),
            _ => id,
        };
        if !matches!(op, HostOp::ScrubDrag(_)) {
            self.busy.set(true);
        }
        apply_enqueue(&mut st.pending, Pending { epoch, op });
        self.begin_drain_locked(&mut st)
    }

    /// Drop pending scrub / scrub-release / play (PreemptScrub). Keeps Open.
    pub fn clear_pending_scrub_and_play(&self) {
        self.inner.borrow_mut().pending.retain(|p| {
            !matches!(
                p.op,
                HostOp::ScrubDrag(_)
                    | HostOp::ScrubDwellExact(_)
                    | HostOp::ScrubRelease { .. }
                    | HostOp::PlayPrime
            )
        });
        self.sync_busy();
    }

    pub fn scrub_inflight(&self) -> bool {
        self.inner.borrow().scrub_inflight
    }

    pub fn set_scrub_inflight(&self, on: bool) {
        self.inner.borrow_mut().scrub_inflight = on;
    }

    /// True when a PlayPrime or `ScrubRelease { then_play }` is waiting.
    pub fn has_pending_play(&self) -> bool {
        self.inner
            .borrow()
            .pending
            .iter()
            .any(|p| p.op.starts_play())
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
        self.inner
            .borrow_mut()
            .pending
            .pop_front()
            .map(|p| (p.epoch, p.op))
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
