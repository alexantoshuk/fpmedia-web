//! Apply [`fpmedia_web::TransportCmd`] against the live player / OpQueue.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::rc::Rc;

use fpmedia_web::{TransportCmd, TransportEvent, TransportModel, TransportState, transport_reduce};
use leptos::prelude::*;
use wasm_bindgen_futures::spawn_local;

use super::ctx::HostCtx;
use super::host_ops::enqueue_seek_step;
use super::op_queue::HostOp;

struct TransportInner {
    model: TransportModel,
    pending_cmds: VecDeque<TransportCmd>,
}

/// Shared transport FSM handle (one per mounted `<Video>`).
#[derive(Clone)]
pub(super) struct TransportHandle {
    inner: Rc<RefCell<TransportInner>>,
    scheduled: Rc<Cell<bool>>,
}

impl TransportHandle {
    /// Finger on the scrubber (drag gesture, not settle).
    pub(super) fn is_scrubbing(&self) -> bool {
        self.inner
            .try_borrow()
            .is_ok_and(|inner| matches!(inner.model.state, TransportState::Scrubbing { .. }))
    }

    pub(super) fn state(&self) -> TransportState {
        self.inner
            .try_borrow()
            .map(|inner| inner.model.state)
            .unwrap_or(TransportState::Closed)
    }

    /// Leave Playing / PlayPriming / scrub without emitting cmds (error stop).
    pub(super) fn force_ready_unless_closed(&self) {
        let Ok(mut inner) = self.inner.try_borrow_mut() else {
            return;
        };
        if !matches!(inner.model.state, TransportState::Closed) {
            inner.model.state = TransportState::Ready;
        }
    }
}

pub(super) fn new_transport() -> TransportHandle {
    TransportHandle {
        inner: Rc::new(RefCell::new(TransportInner {
            model: TransportModel::new(),
            pending_cmds: VecDeque::new(),
        })),
        scheduled: Rc::new(Cell::new(false)),
    }
}

/// Mirror [`TransportState::ui_playing`] onto the UI `playing` signal.
pub(super) fn sync_playing_from_transport(ctx: &HostCtx) {
    let live = ctx.with_player(|p| p.play_session_live()).unwrap_or(false);
    let ui = ctx.transport.state().ui_playing(live);
    ctx.video.playing.set(ui);
}

/// Run reduce + queue engine cmds. Playhead updates sync; engine work is serialized
/// on one microtask drain (pointer Move is rate-limited in `<Video>` via leptos-use).
pub(super) fn dispatch_transport(ctx: &HostCtx, kick_drain: &Rc<dyn Fn()>, event: TransportEvent) {
    let v = ctx.video;
    let engine_cmds = {
        let mut inner = ctx.transport.inner.borrow_mut();
        let (next, cmds) = transport_reduce(inner.model, event);
        inner.model = next;
        let mut engine = Vec::new();
        for cmd in cmds {
            match cmd {
                TransportCmd::SetPlayhead { frame } => {
                    v.playhead.set(frame);
                    v.ended.set(false);
                }
                TransportCmd::ClearEnded => v.ended.set(false),
                other => engine.push(other),
            }
        }
        for c in engine {
            inner.pending_cmds.push_back(c);
        }
        !inner.pending_cmds.is_empty()
    };
    sync_playing_from_transport(ctx);

    if !engine_cmds {
        return;
    }
    schedule_cmd_drain(ctx, kick_drain);
}

fn schedule_cmd_drain(ctx: &HostCtx, kick_drain: &Rc<dyn Fn()>) {
    let transport = &ctx.transport;
    if transport.scheduled.get() {
        return;
    }
    transport.scheduled.set(true);
    let ctx = ctx.clone();
    let kick_drain = Rc::clone(kick_drain);
    spawn_local(async move {
        let transport = &ctx.transport;
        loop {
            let cmd = transport.inner.borrow_mut().pending_cmds.pop_front();
            let Some(cmd) = cmd else {
                break;
            };
            apply_engine_cmd(&ctx, &kick_drain, cmd);
        }
        transport.scheduled.set(false);
        if !transport.inner.borrow().pending_cmds.is_empty() {
            schedule_cmd_drain(&ctx, &kick_drain);
        }
    });
}

fn apply_engine_cmd(ctx: &HostCtx, kick_drain: &Rc<dyn Fn()>, cmd: TransportCmd) {
    let HostCtx { queue, player, .. } = ctx;
    let playhead = ctx.video.playhead;
    match cmd {
        TransportCmd::SetPlayhead { .. } | TransportCmd::ClearEnded => {}
        TransportCmd::SoftPause {
            align_playhead,
            densify,
        } => {
            if let Some(p) = player.borrow().as_ref().cloned() {
                if align_playhead {
                    // Same as HardPause `stop_playback`: UI playhead lags ~100ms
                    // during play. Without this snap, PlayPrime seeks Exact on
                    // the stale aim and the soft-hold warm resume is wasted.
                    let eng = p.state().frame.min(u64::from(u32::MAX)) as u32;
                    if playhead.get_untracked() != eng {
                        web_sys::console::log_1(
                            &format!(
                                "fpmedia-web: soft pause align playhead {} → {eng}",
                                playhead.get_untracked()
                            )
                            .into(),
                        );
                    }
                    playhead.set(eng);
                }
                // Always soft-hold when the cmd fires (even if UI already synced
                // off Playing) — matches prior SoftPause; clock/session may still
                // be live for one microtask after transport left Playing.
                // Pause button: densify idle residency. Soft-drag: densify off
                // (Exact under finger is ScrubDrag → main HW).
                web_sys::console::log_1(
                    &format!("fpmedia-web: soft pause (warm audio, densify={densify})").into(),
                );
                if densify {
                    p.pause_warm_idle();
                } else {
                    p.pause_for_scrub();
                }
            }
        }
        TransportCmd::HardPause => {
            ctx.stop_playback();
        }
        TransportCmd::PreemptScrub => {
            // Bump so in-flight ScrubRelease / PlayPrime bails and does not note
            // a stale Done that would leave transport in Playing while UI paused.
            queue.bump();
            if let Some(p) = player.borrow().as_ref().cloned() {
                p.cancel_ops();
            }
            queue.clear_pending_scrub_and_play();
        }
        TransportCmd::EnqueueDrag { frame } => {
            enqueue_scrub_drag(ctx, kick_drain, frame);
        }
        TransportCmd::EnqueueScrubRelease {
            frame,
            then_play,
            settle_gen,
        } => {
            enqueue_scrub_release(ctx, kick_drain, frame, then_play, settle_gen);
        }
        TransportCmd::EnqueuePlayPrime => {
            if queue.enqueue(HostOp::PlayPrime) {
                kick_drain();
            }
        }
        TransportCmd::EnqueueSeek { frame } => {
            enqueue_seek_step(ctx, kick_drain, HostOp::Seek(frame));
        }
        TransportCmd::EnqueueSeekKeyframe { frame } => {
            enqueue_seek_step(ctx, kick_drain, HostOp::SeekKeyframe(frame));
        }
        TransportCmd::EnqueueStep { forward } => {
            enqueue_seek_step(ctx, kick_drain, HostOp::Step(forward));
        }
    }
}

/// Leave play for a scrub op: soft-hold the engine.
/// Returns the live player, or `None` when nothing is open.
fn leave_play_for_scrub(ctx: &HostCtx) -> Option<Rc<fpmedia_web::VideoPlayer>> {
    let p = ctx.player.borrow().as_ref().cloned()?;
    if p.is_playing() || p.play_session_live() {
        p.pause_for_scrub();
    }
    Some(p)
}

fn enqueue_scrub_drag(ctx: &HostCtx, kick_drain: &Rc<dyn Fn()>, frame: u32) {
    let Some(p) = leave_play_for_scrub(ctx) else {
        return;
    };
    p.clear_densify_hold();
    // Densify off via `note_scrub_frame` — do **not** `cancel_ops` here: that
    // aborted in-flight main-HW key preview every ~50 ms. Settle / dwell still
    // `prepare_scrub_settle`. **no equivalent** (scrub local).
    p.note_scrub_frame(u64::from(frame));
    if ctx.queue.enqueue(HostOp::ScrubDrag(frame)) {
        kick_drain();
    }
}

fn enqueue_scrub_release(
    ctx: &HostCtx,
    kick_drain: &Rc<dyn Fn()>,
    frame: u32,
    then_play: bool,
    settle_gen: u64,
) {
    let Some(p) = leave_play_for_scrub(ctx) else {
        return;
    };
    p.prepare_scrub_settle(u64::from(frame));
    if ctx.queue.enqueue(HostOp::ScrubRelease {
        frame,
        then_play,
        settle_gen,
    }) {
        kick_drain();
    }
}

/// Soft-drag stillness → Exact aim while finger still down (no transport Settling).
///
/// Cache-hit paints sync; miss runs the same settle path as pointer-up.
/// **no equivalent** (scrub local).
pub(super) fn enqueue_scrub_dwell_exact(ctx: &HostCtx, kick_drain: &Rc<dyn Fn()>, frame: u32) {
    if !ctx.transport.is_scrubbing() {
        return;
    }
    let Some(p) = leave_play_for_scrub(ctx) else {
        return;
    };
    if p.try_present_scrub_exact(u64::from(frame)) {
        // Already Exact under finger — skip settle churn; allow densify while still.
        p.arm_scrub_densify_after_exact(u64::from(frame));
        ctx.sync_ui();
        return;
    }
    p.prepare_scrub_settle(u64::from(frame));
    if ctx.queue.enqueue(HostOp::ScrubDwellExact(frame)) {
        kick_drain();
    }
}

/// Notify FSM after an async op completes (should not emit engine cmds).
pub(super) fn note_transport(ctx: &HostCtx, event: TransportEvent) {
    {
        let mut inner = ctx.transport.inner.borrow_mut();
        let (next, cmds) = transport_reduce(inner.model, event);
        debug_assert!(
            cmds.is_empty(),
            "transport note event should not emit cmds: {event:?} -> {cmds:?}"
        );
        inner.model = next;
    }
    sync_playing_from_transport(ctx);
}
