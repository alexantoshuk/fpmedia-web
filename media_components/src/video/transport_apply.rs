//! Apply [`webmedia::TransportCmd`] against the live player / OpQueue.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::rc::Rc;

use leptos::prelude::*;
use wasm_bindgen_futures::spawn_local;
use webmedia::{TransportCmd, TransportEvent, TransportModel, transport_reduce};

use super::host_ops::PlayerHandle;
use super::op_queue::OpQueue;
use super::session::PlaySession;

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

pub(super) fn new_transport() -> TransportHandle {
    TransportHandle {
        inner: Rc::new(RefCell::new(TransportInner {
            model: TransportModel::new(),
            pending_cmds: VecDeque::new(),
        })),
        scheduled: Rc::new(Cell::new(false)),
    }
}

/// Run reduce + queue engine cmds. Playhead updates sync; engine work is serialized
/// on one microtask drain (same race class as the old demo `defer_scrub` TLS).
#[allow(clippy::too_many_arguments)] // host wiring: transport + queue + UI signals
pub(super) fn dispatch_transport(
    transport: &TransportHandle,
    event: TransportEvent,
    queue: &OpQueue,
    kick_drain: &Rc<dyn Fn()>,
    player: &PlayerHandle,
    session: &Rc<RefCell<Option<PlaySession>>>,
    playing: RwSignal<bool>,
    playhead: RwSignal<u32>,
    ended: RwSignal<bool>,
    stop_playback: &Rc<dyn Fn()>,
) {
    let engine_cmds = {
        let mut inner = transport.inner.borrow_mut();
        let (next, cmds) = transport_reduce(inner.model, event);
        inner.model = next;
        let mut engine = Vec::new();
        for cmd in cmds {
            match cmd {
                TransportCmd::SetPlayhead { frame } => {
                    playhead.set(frame);
                    ended.set(false);
                }
                TransportCmd::ClearEnded => ended.set(false),
                other => engine.push(other),
            }
        }
        for c in engine {
            inner.pending_cmds.push_back(c);
        }
        !inner.pending_cmds.is_empty()
    };

    if !engine_cmds {
        return;
    }
    schedule_cmd_drain(
        transport,
        queue,
        kick_drain,
        player,
        session,
        playing,
        stop_playback,
    );
}

fn schedule_cmd_drain(
    transport: &TransportHandle,
    queue: &OpQueue,
    kick_drain: &Rc<dyn Fn()>,
    player: &PlayerHandle,
    session: &Rc<RefCell<Option<PlaySession>>>,
    playing: RwSignal<bool>,
    stop_playback: &Rc<dyn Fn()>,
) {
    if transport.scheduled.get() {
        return;
    }
    transport.scheduled.set(true);
    let transport = transport.clone();
    let queue = queue.clone();
    let kick_drain = Rc::clone(kick_drain);
    let player = Rc::clone(player);
    let session = Rc::clone(session);
    let stop_playback = Rc::clone(stop_playback);
    spawn_local(async move {
        loop {
            let cmd = transport.inner.borrow_mut().pending_cmds.pop_front();
            let Some(cmd) = cmd else {
                break;
            };
            apply_engine_cmd(
                cmd,
                &queue,
                &kick_drain,
                &player,
                &session,
                playing,
                &stop_playback,
            );
        }
        transport.scheduled.set(false);
        if !transport.inner.borrow().pending_cmds.is_empty() {
            schedule_cmd_drain(
                &transport,
                &queue,
                &kick_drain,
                &player,
                &session,
                playing,
                &stop_playback,
            );
        }
    });
}

fn apply_engine_cmd(
    cmd: TransportCmd,
    queue: &OpQueue,
    kick_drain: &Rc<dyn Fn()>,
    player: &PlayerHandle,
    session: &Rc<RefCell<Option<PlaySession>>>,
    playing: RwSignal<bool>,
    stop_playback: &Rc<dyn Fn()>,
) {
    match cmd {
        TransportCmd::SetPlayhead { .. } | TransportCmd::ClearEnded => {}
        TransportCmd::SoftPauseForScrub => {
            if playing.get_untracked() {
                playing.set(false);
                let _ = session.borrow_mut().take();
            }
            if let Some(p) = player.borrow().as_ref().cloned() {
                p.pause_for_scrub();
            }
        }
        TransportCmd::HardPause => {
            stop_playback();
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
            enqueue_scrub_drag(queue, kick_drain, player, session, playing, frame);
        }
        TransportCmd::EnqueueScrubRelease {
            frame,
            then_play,
            settle_gen,
        } => {
            enqueue_scrub_release(
                queue, kick_drain, player, session, playing, frame, then_play, settle_gen,
            );
        }
        TransportCmd::EnqueuePlayPrime => {
            if queue.enqueue_play() {
                kick_drain();
            }
        }
    }
}

fn enqueue_scrub_drag(
    queue: &OpQueue,
    kick_drain: &Rc<dyn Fn()>,
    player: &PlayerHandle,
    session: &Rc<RefCell<Option<PlaySession>>>,
    playing: RwSignal<bool>,
    frame: u32,
) {
    if player.borrow().as_ref().is_none() {
        return;
    }
    let leaving_play = playing.get_untracked();
    if leaving_play {
        playing.set(false);
        let _ = session.borrow_mut().take();
    }
    if let Some(p) = player.borrow().as_ref().cloned() {
        if leaving_play {
            p.pause_for_scrub();
        }
        p.clear_densify_hold();
        p.cancel_ops();
        p.note_scrub_frame(u64::from(frame));
    }
    if queue.enqueue_scrub_drag(frame) {
        kick_drain();
    }
}

#[allow(clippy::too_many_arguments)] // host wiring: queue + session + play UI
pub(super) fn enqueue_scrub_release(
    queue: &OpQueue,
    kick_drain: &Rc<dyn Fn()>,
    player: &PlayerHandle,
    session: &Rc<RefCell<Option<PlaySession>>>,
    playing: RwSignal<bool>,
    frame: u32,
    then_play: bool,
    settle_gen: u64,
) {
    if player.borrow().as_ref().is_none() {
        return;
    }
    let leaving_play = playing.get_untracked();
    if leaving_play {
        playing.set(false);
        let _ = session.borrow_mut().take();
    }
    if let Some(p) = player.borrow().as_ref().cloned() {
        if leaving_play {
            p.pause_for_scrub();
        }
        p.prepare_scrub_settle(u64::from(frame));
    }
    if queue.enqueue_scrub_release(frame, then_play, settle_gen) {
        kick_drain();
    }
}

/// Notify FSM after an async op completes (should not emit engine cmds).
pub(super) fn note_transport(transport: &TransportHandle, event: TransportEvent) {
    let mut inner = transport.inner.borrow_mut();
    let (next, cmds) = transport_reduce(inner.model, event);
    debug_assert!(
        cmds.is_empty(),
        "transport note event should not emit cmds: {event:?} -> {cmds:?}"
    );
    inner.model = next;
}
