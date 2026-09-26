//! Host-op enqueue helpers + the async bodies the drain loop runs.

use std::cell::RefCell;
use std::rc::Rc;

use leptos::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen::closure::Closure;
use wasm_bindgen_futures::spawn_local;
use web_sys::HtmlCanvasElement;
use webmedia::{MediaSource, OpenPhase, VideoPlayer};

use super::op_queue::{HostOp, OpQueue};
use super::open_url::open_source_staged;
use super::session::{FpsMeter, PlaySession, performance_now_ms};

pub(super) type PlayerHandle = Rc<RefCell<Option<Rc<VideoPlayer>>>>;

pub(super) fn enqueue_seek_step(
    queue: &OpQueue,
    kick_drain: &Rc<dyn Fn()>,
    player: &PlayerHandle,
    session: &Rc<RefCell<Option<PlaySession>>>,
    playing: RwSignal<bool>,
    op: HostOp,
) {
    if player.borrow().as_ref().is_none() {
        return;
    }
    // Mirror Pause: clear stale playing so controls stay usable.
    playing.set(false);
    let _ = session.borrow_mut().take();
    if let Some(p) = player.borrow().as_ref().cloned() {
        // Sync Prev/Next/Seek wait for each other, but preempt async scrub.
        if queue.scrub_inflight() {
            p.cancel_ops();
        }
        p.pause();
    }
    if queue.enqueue_seek_or_step(op) {
        kick_drain();
    }
}

pub(super) enum SeekOrStep {
    Seek(u32),
    SeekKeyframe(u32),
    Scrub { target: u32, settle: bool },
    ScrubRelease { target: u32, then_play: bool },
    Step(bool),
}

#[allow(clippy::too_many_arguments)] // Leptos open task wires many signals.
pub(super) async fn run_open(
    queue: &OpQueue,
    id: u64,
    source: std::sync::Arc<dyn MediaSource>,
    canvas: HtmlCanvasElement,
    player: &PlayerHandle,
    sync_ui: &Rc<dyn Fn()>,
    fps_meter: &Rc<RefCell<FpsMeter>>,
    ready: RwSignal<bool>,
    metadata: RwSignal<Option<webmedia::VideoMetadata>>,
    error: RwSignal<Option<String>>,
    status: RwSignal<String>,
    fps: RwSignal<Option<f64>>,
    pipeline: RwSignal<Option<webmedia::PipelineStats>>,
    has_audio: RwSignal<bool>,
    volume: RwSignal<f64>,
    muted: RwSignal<bool>,
    playback_rate: RwSignal<f64>,
) {
    web_sys::console::log_1(&"webmedia: open task running".into());
    if !queue.is_current(id) {
        return;
    }
    *player.borrow_mut() = None;
    let opened = open_source_staged(source, canvas, move |phase: OpenPhase| {
        let msg = phase.label();
        web_sys::console::log_1(&format!("webmedia: status {msg}").into());
        status.set(msg);
    })
    .await;
    if !queue.is_current(id) {
        return;
    }
    match opened {
        Ok(p) => {
            let p = Rc::new(p);
            metadata.set(Some(p.metadata()));
            has_audio.set(p.has_audio());
            p.set_playback_rate(playback_rate.get_untracked());
            if p.has_audio() {
                p.set_volume(volume.get_untracked());
                p.set_muted(muted.get_untracked());
            }
            let _ = p.relayout();
            *player.borrow_mut() = Some(Rc::clone(&p));
            status.set(OpenPhase::DecodingStart.label());
            let seek = p.seek_frame(0).await;
            if !queue.is_current(id) {
                return;
            }
            match seek {
                Ok(()) => {
                    ready.set(true);
                    status.set(OpenPhase::Ready.label());
                    fps_meter.borrow_mut().reset();
                    fps.set(None);
                    pipeline.set(Some(p.pipeline_stats()));
                    sync_ui();
                }
                Err(e) if e.is_cancelled() => {
                    status.set(OpenPhase::Cancelled.label());
                }
                Err(e) => {
                    ready.set(false);
                    error.set(Some(e.to_string()));
                    status.set(OpenPhase::SeekStartFailed.label());
                }
            }
        }
        Err(e) => {
            ready.set(false);
            metadata.set(None);
            error.set(Some(e.clone()));
            status.set(OpenPhase::Failed.label());
            web_sys::console::error_1(&e.clone().into());
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn run_seek_or_step(
    queue: &OpQueue,
    id: u64,
    player: &PlayerHandle,
    sync_ui: &Rc<dyn Fn()>,
    session: &Rc<RefCell<Option<PlaySession>>>,
    error: RwSignal<Option<String>>,
    playing: RwSignal<bool>,
    playhead: RwSignal<u32>,
    op: SeekOrStep,
) -> bool {
    if !queue.is_current(id) {
        return false;
    }
    let Some(p) = player.borrow().as_ref().cloned() else {
        return false;
    };
    let _ = session.borrow_mut().take();
    playing.set(false);
    // Scrub settle: host already soft-paused on pointerdown and `prepare_scrub_settle`
    // latched densify hold. Calling hard `pause()` cleared that hold and re-kicked
    // densify under exact goto → `decoder busy` / resume Play no-op (URL fMP4).
    let scrub_settle = matches!(
        &op,
        SeekOrStep::Scrub { settle: true, .. } | SeekOrStep::ScrubRelease { .. }
    );
    if scrub_settle {
        if p.is_playing() {
            p.pause_for_scrub();
        }
    } else {
        p.pause();
    }
    let label = match &op {
        SeekOrStep::Seek(t) => format!("seek {t}"),
        SeekOrStep::SeekKeyframe(t) => format!("seek-key {t}"),
        SeekOrStep::Scrub { target: t, settle } => {
            if *settle {
                format!("scrub settle {t}")
            } else {
                format!("scrub {t}")
            }
        }
        SeekOrStep::ScrubRelease {
            target: t,
            then_play,
        } => {
            if *then_play {
                format!("scrub release+play {t}")
            } else {
                format!("scrub release {t}")
            }
        }
        SeekOrStep::Step(true) => "next".into(),
        SeekOrStep::Step(false) => "prev".into(),
    };
    web_sys::console::log_1(&format!("webmedia: {label} start").into());
    let is_step = matches!(op, SeekOrStep::Step(_));
    let result = match op {
        SeekOrStep::Seek(target) => p.seek_frame(u64::from(target)).await,
        SeekOrStep::SeekKeyframe(target) => p.seek_keyframe(u64::from(target)).await,
        SeekOrStep::Scrub { target, settle } => {
            let r = p.scrub_to(u64::from(target), settle).await;
            // Settle no longer arms densify inside the engine — if Play is queued,
            // keep the warm main decoder for prime (play-after-scrub).
            if settle && r.is_ok() && queue.is_current(id) && !queue.has_pending_play() {
                p.arm_scrub_densify_after_exact(u64::from(target));
            }
            r
        }
        SeekOrStep::ScrubRelease { target, then_play } => {
            let r = p.scrub_to(u64::from(target), true).await;
            if r.is_ok() && queue.is_current(id) && !then_play && !queue.has_pending_play() {
                p.arm_scrub_densify_after_exact(u64::from(target));
            }
            r
        }
        SeekOrStep::Step(true) => p.next_frame().await,
        SeekOrStep::Step(false) => p.previous_frame().await,
    };
    if !queue.is_current(id) {
        return false;
    }
    let done = match &result {
        Ok(()) => "ok",
        Err(e) if e.is_cancelled() => "cancel",
        Err(_) => "err",
    };
    web_sys::console::log_1(&format!("webmedia: {label} done {done}").into());
    if let Err(e) = &result
        && !e.is_cancelled()
    {
        error.set(Some(e.to_string()));
    }
    sync_ui();
    // Step can no-op (warm blit miss); snap playhead to committed engine so hold
    // repeat does not keep aiming one ahead of a stuck frame.
    if is_step
        && let Ok(borrow) = player.try_borrow()
        && let Some(p) = borrow.as_ref()
    {
        let engine = p.state().frame.min(u32::MAX as u64) as u32;
        playhead.set(engine);
    }
    playing.set(false);
    result.is_ok()
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn run_play_prime(
    queue: &OpQueue,
    id: u64,
    player: &PlayerHandle,
    session: &Rc<RefCell<Option<PlaySession>>>,
    fps_meter: &Rc<RefCell<FpsMeter>>,
    stop_playback: &Rc<dyn Fn()>,
    resume_raf: &impl Fn(),
    ready: RwSignal<bool>,
    playing: RwSignal<bool>,
    playhead: RwSignal<u32>,
    error: RwSignal<Option<String>>,
    fps: RwSignal<Option<f64>>,
) -> bool {
    if !queue.is_current(id) || !ready.get_untracked() {
        return false;
    }
    let Some(p) = player.borrow().as_ref().cloned() else {
        return false;
    };

    // Sync engine to UI playhead only if still mismatched (e.g. settle cancelled
    // by seek/step, or play without a prior settle). Do this *before* `play()` so
    // we do not cancel an in-flight scrub that was already targeting playhead —
    // that op finished ahead of us in the drain queue.
    //
    // Cold scrub soft-settle may leave UI playhead on an unindexed / uncached
    // aim while engine committed the indexed prefix. Priming that aim blocked
    // OpQueue (`warm_sample_bytes` / mfra) and stuck Play `busy`. Snap to a
    // playable frame first (mediabunny: first samples before play).
    let indexed_last = p
        .indexed_frame_count()
        .saturating_sub(1)
        .min(u64::from(u32::MAX)) as u32;
    let eng = p.state().frame.min(u64::from(u32::MAX)) as u32;
    let mut ph = playhead.get_untracked().min(indexed_last);
    if !p.frame_exact_ready(u64::from(ph)) {
        let safe = eng.min(indexed_last);
        if safe != ph {
            web_sys::console::log_1(
                &format!("webmedia: play snap cold playhead {ph} → {safe}").into(),
            );
            playhead.set(safe);
            ph = safe;
        }
    }
    if p.state().frame != u64::from(ph) {
        web_sys::console::log_1(
            &format!("webmedia: play sync seek {ph} (engine {})", p.state().frame).into(),
        );
        match p.seek_frame(u64::from(ph)).await {
            Ok(()) => {}
            Err(e) if e.is_cancelled() => return false,
            Err(e) => {
                error.set(Some(e.to_string()));
                return false;
            }
        }
        if !queue.is_current(id) {
            return false;
        }
    }

    p.play();
    fps_meter.borrow_mut().reset();
    fps.set(None);
    // Defer UI `playing` until after prime + media clock. Setting it earlier lets
    // rAF call `present_due` with a scrub-stale clock (often still at EOF after
    // Ended→scrub); that returns `DuePresent::Ended` → `stop_playback` bumps
    // async_id and silently aborts this PlayPrime.
    let still_queued = || queue.is_current(id);

    // Unlock AudioContext during the Play click turn — before long prime awaits.
    if let Err(e) = p.resume_audio_context().await {
        web_sys::console::warn_1(&format!("webmedia: audio resume: {e}").into());
    }
    if !still_queued() {
        if p.is_playing() {
            p.pause();
        }
        return false;
    }

    // Soft-park after prime/clock failure: must NOT call `stop_playback()` —
    // that bumps async_id so the drain skips `PlayPrimeFailed` and transport
    // stays deaf in `PlayPriming` (stress: Play did not start after scrub).
    // mediabunny: examples/media-player play() after seekToTime — no priming trap.
    let park_failed_prime = || {
        let _ = session.borrow_mut().take();
        playing.set(false);
        if p.is_playing() {
            p.pause();
        }
        fps_meter.borrow_mut().reset();
        fps.set(None);
    };

    web_sys::console::log_1(&"webmedia: play prime start".into());
    let prime_ok = match p.prime_playback().await {
        Ok(()) => true,
        Err(e) if e.is_cancelled() => false,
        Err(e) => {
            if still_queued() {
                error.set(Some(e.to_string()));
            }
            false
        }
    };
    web_sys::console::log_1(&format!("webmedia: play prime done ok={prime_ok}").into());

    if !still_queued() {
        if p.is_playing() {
            p.pause();
        }
        return false;
    }
    if !prime_ok {
        park_failed_prime();
        return false;
    }

    let media_origin = p.state().timestamp.seconds;
    // Scrub-while-play soft-held the AudioDecoder — warm restart (no
    // isConfigSupported). Fresh Pause→Play stays on cold start_media_clock.
    // mediabunny: examples/media-player play() → buffers(t) after pause seek.
    let clock = if p.take_audio_warm_resume() {
        web_sys::console::log_1(&"webmedia: play clock warm resume".into());
        p.resume_media_clock(media_origin).await
    } else {
        p.start_media_clock(media_origin).await
    };
    if let Err(e) = clock {
        if still_queued() {
            error.set(Some(e.to_string()));
            park_failed_prime();
        }
        return false;
    }
    if !still_queued() {
        if p.is_playing() {
            p.pause();
        }
        return false;
    }
    let last_ui_ms = performance_now_ms().unwrap_or(0.0);

    *session.borrow_mut() = Some(PlaySession {
        media_origin,
        wall_origin: performance_now_ms().map(|t| t / 1000.0).unwrap_or(0.0),
        last_ui_ms,
        restarting: false,
        holding_for_buffer: false,
        resuming_clock: false,
    });
    playing.set(true);

    {
        let player = Rc::clone(player);
        let session = Rc::clone(session);
        let stop_playback = Rc::clone(stop_playback);
        let queue = queue.clone();
        spawn_local(async move {
            while playing.get_untracked() && queue.is_current(id) {
                let pause_prefetch = session
                    .borrow()
                    .as_ref()
                    .is_some_and(|s| s.restarting || s.resuming_clock);
                let need = {
                    let borrow = player.borrow();
                    borrow.as_ref().is_some_and(|p| {
                        !p.is_restarting() && !pause_prefetch && p.wants_play_prefetch()
                    })
                };
                if need {
                    let decoded = {
                        let Some(p) = player.borrow().as_ref().cloned() else {
                            break;
                        };
                        p.prefetch_one().await
                    };
                    match decoded {
                        Ok(true) => continue,
                        Ok(false) => {}
                        Err(e) if e.is_cancelled() => break,
                        Err(e) => {
                            if queue.is_current(id) {
                                error.set(Some(e.to_string()));
                                stop_playback();
                            }
                            break;
                        }
                    }
                }
                {
                    let prepare = {
                        let Some(p) = player.borrow().as_ref().cloned() else {
                            break;
                        };
                        if p.is_looping() {
                            Some(p.prepare_loop_if_needed().await)
                        } else {
                            None
                        }
                    };
                    match prepare {
                        Some(Err(e)) if e.is_cancelled() => break,
                        Some(Err(e)) => {
                            if queue.is_current(id) {
                                error.set(Some(e.to_string()));
                                stop_playback();
                            }
                            break;
                        }
                        _ => {}
                    }
                }
                wait_animation_frame().await;
            }
        });
    }

    if still_queued() && playing.get_untracked() {
        resume_raf();
    }
    still_queued() && playing.get_untracked()
}

/// Await one animation frame with a dedicated Closure (not Promise.resolve).
pub(super) async fn wait_animation_frame() {
    let promise = js_sys::Promise::new(&mut |resolve, _reject| {
        let window = web_sys::window().expect("window");
        let resolve = resolve.clone();
        let cb = Closure::once(move |_t: f64| {
            let _ = resolve.call0(&wasm_bindgen::JsValue::NULL);
        });
        let _ = window.request_animation_frame(cb.as_ref().unchecked_ref());
        cb.forget();
    });
    let _ = wasm_bindgen_futures::JsFuture::from(promise).await;
}
