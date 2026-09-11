//! [`Video`] component — canvas stage + playback orchestration.

use std::cell::RefCell;
use std::rc::Rc;

use leptos::html::{Canvas, Div};
use leptos::prelude::*;
use leptos_use::utils::Pausable;
use leptos_use::{use_raf_fn_with_options, use_resize_observer, UseRafFnOptions};
use wasm_bindgen::JsCast;
use wasm_bindgen::closure::Closure;
use wasm_bindgen_futures::spawn_local;
use web_sys::HtmlCanvasElement;
use webmedia::{
    BlobSource, DuePresent, OpenMp4Options, WebPlayer, open_mp4_source_cached_with_progress,
};

use crate::op_queue::{HostOp, OpQueue};
use crate::{VideoOps, VideoRef};

type PlayerHandle = Rc<RefCell<Option<Rc<WebPlayer>>>>;

/// Media-clock + UI throttle state for an active play session.
struct PlaySession {
    media_origin: f64,
    wall_origin: f64,
    last_ui_ms: f64,
    /// `loop_to_start` in flight — skip presents until origins reset.
    restarting: bool,
}

/// Rolling window of canvas presents → measured display FPS.
struct FpsMeter {
    presents: Vec<f64>,
}

impl FpsMeter {
    fn new() -> Self {
        Self {
            presents: Vec::new(),
        }
    }

    fn reset(&mut self) {
        self.presents.clear();
    }

    fn note_present(&mut self) -> Option<f64> {
        let now = performance_now_ms()?;
        self.presents.push(now);
        let cutoff = now - 1000.0;
        self.presents.retain(|&t| t >= cutoff);
        if self.presents.len() < 2 {
            return None;
        }
        let first = *self.presents.first()?;
        let span_ms = now - first;
        if span_ms <= 0.0 {
            return None;
        }
        Some((self.presents.len() - 1) as f64 * 1000.0 / span_ms)
    }
}

/// Frame-accurate media element (HTML `<video>` analog).
///
/// Renders only the media stage. Drive it through [`VideoRef`].
#[component]
pub fn Video(
    video: VideoRef,
    #[prop(optional, into)] class: Option<String>,
    #[prop(optional, into)] viewport_class: Option<String>,
    #[prop(optional, into)] canvas_class: Option<String>,
) -> impl IntoView {
    let canvas_ref = NodeRef::<Canvas>::new();
    let viewport_ref = NodeRef::<Div>::new();
    let player: PlayerHandle = Rc::new(RefCell::new(None));
    let fps_meter = Rc::new(RefCell::new(FpsMeter::new()));
    let session: Rc<RefCell<Option<PlaySession>>> = Rc::new(RefCell::new(None));

    let busy = video.busy;
    let ready = video.ready;
    let playing = video.playing;
    let looping = video.looping;
    let frame = video.frame;
    let frame_count = video.frame_count;
    let timestamp = video.timestamp;
    let fps = video.fps;
    let metadata = video.metadata;
    let error = video.error;
    let pipeline = video.pipeline;
    let status = video.status;

    // mediabunny CallSerializer + asyncId — one queue for open / seek / play-prime.
    let queue = OpQueue::new(busy);

    let sync_ui = {
        let player = Rc::clone(&player);
        Rc::new(move || {
            let Ok(borrow) = player.try_borrow() else {
                return;
            };
            if let Some(p) = borrow.as_ref() {
                let st = p.state();
                frame.set(st.frame.min(u32::MAX as u64) as u32);
                frame_count.set(st.frame_count.min(u32::MAX as u64) as u32);
                timestamp.set(st.timestamp.seconds);
                // Do NOT sync `playing` from the engine. UI owns transport state;
                // overwriting after Pause (e.g. pause missed try_borrow, then
                // sync_ui) left playing=true with no session → Play/Next/Prev dead.
                looping.set(st.looping);
                pipeline.set(Some(p.pipeline_stats()));
            }
        }) as Rc<dyn Fn()>
    };

    let stop_playback = {
        let player = Rc::clone(&player);
        let session = Rc::clone(&session);
        let fps_meter = Rc::clone(&fps_meter);
        let sync_ui = Rc::clone(&sync_ui);
        let queue = queue.clone();
        Rc::new(move || {
            // Abort in-flight play-prime / prefetch; drop queued PlayPrime.
            queue.bump();
            queue.clear_pending_play();
            playing.set(false);
            let _ = session.borrow_mut().take();
            let p = player.borrow().as_ref().cloned();
            if let Some(p) = p {
                p.pause();
            }
            fps_meter.borrow_mut().reset();
            fps.set(None);
            sync_ui();
            playing.set(false);
            // `busy` stays with the op queue (open/seek may still be draining).
        }) as Rc<dyn Fn()>
    };

    let Pausable {
        pause: pause_raf,
        resume: resume_raf,
        ..
    } = use_raf_fn_with_options(
        {
            let player = Rc::clone(&player);
            let session = Rc::clone(&session);
            let fps_meter = Rc::clone(&fps_meter);
            let sync_ui = Rc::clone(&sync_ui);
            let stop_playback = Rc::clone(&stop_playback);
            move |_| {
                if !playing.get_untracked() {
                    return;
                }

                let restarting = session
                    .borrow()
                    .as_ref()
                    .is_some_and(|s| s.restarting);
                if restarting {
                    return;
                }

                {
                    let want = looping.get_untracked();
                    if let Ok(borrow) = player.try_borrow() {
                        if let Some(p) = borrow.as_ref() {
                            p.set_loop(want);
                        }
                    }
                }

                let t = {
                    let borrow = session.borrow();
                    let Some(s) = borrow.as_ref() else {
                        return;
                    };
                    // Wall-clock media timeline until an audio track drives the clock.
                    if let Some(now) = performance_now_ms() {
                        now / 1000.0 - s.wall_origin + s.media_origin
                    } else {
                        s.media_origin
                    }
                };

                let outcome = {
                    let borrow = player.borrow();
                    match borrow.as_ref() {
                        Some(p) => p.present_due(t),
                        None => {
                            stop_playback();
                            return;
                        }
                    }
                };

                match outcome {
                    Ok(DuePresent::Ended) => {
                        stop_playback();
                    }
                    Ok(DuePresent::NeedsLoop) => {
                        if let Some(s) = session.borrow_mut().as_mut() {
                            s.restarting = true;
                        }
                        let player = Rc::clone(&player);
                        let session = Rc::clone(&session);
                        let stop_playback = Rc::clone(&stop_playback);
                        spawn_local(async move {
                            let Some(p) = player.borrow().as_ref().cloned() else {
                                stop_playback();
                                return;
                            };
                            match p.loop_to_start().await {
                                Ok(()) => {
                                    if let Some(s) = session.borrow_mut().as_mut() {
                                        s.media_origin = 0.0;
                                        s.wall_origin = performance_now_ms()
                                            .map(|n| n / 1000.0)
                                            .unwrap_or(0.0);
                                        s.restarting = false;
                                    }
                                }
                                Err(e) if e.is_cancelled() => {
                                    if let Some(s) = session.borrow_mut().as_mut() {
                                        s.restarting = false;
                                    }
                                }
                                Err(e) => {
                                    error.set(Some(e.to_string()));
                                    stop_playback();
                                }
                            }
                        });
                    }
                    Ok(DuePresent::Playing { did_draw }) => {
                        if did_draw {
                            if let Some(measured) = fps_meter.borrow_mut().note_present() {
                                let now_ms = performance_now_ms();
                                let should_publish = {
                                    let borrow = session.borrow();
                                    borrow.as_ref().is_some_and(|s| {
                                        now_ms.is_some_and(|n| n - s.last_ui_ms >= 100.0)
                                    })
                                };
                                if should_publish {
                                    fps.set(Some(measured));
                                }
                            }
                        }
                        if let Some(now) = performance_now_ms() {
                            let mut borrow = session.borrow_mut();
                            if let Some(s) = borrow.as_mut() {
                                if now - s.last_ui_ms >= 100.0 {
                                    s.last_ui_ms = now;
                                    drop(borrow);
                                    sync_ui();
                                }
                            }
                        }
                    }
                    Err(e) => {
                        error.set(Some(e.to_string()));
                        stop_playback();
                    }
                }
            }
        },
        UseRafFnOptions::default().immediate(false),
    );

    Effect::new({
        let pause_raf = pause_raf.clone();
        move |_| {
            if !playing.get() {
                pause_raf();
            }
        }
    });

    use_resize_observer(viewport_ref, {
        let player = Rc::clone(&player);
        move |_entries, _observer| {
            let Ok(borrow) = player.try_borrow() else {
                return;
            };
            if let Some(p) = borrow.as_ref() {
                let _ = p.relayout();
            }
        }
    });

    let kick_drain = {
        let queue = queue.clone();
        let player = Rc::clone(&player);
        let session = Rc::clone(&session);
        let fps_meter = Rc::clone(&fps_meter);
        let sync_ui = Rc::clone(&sync_ui);
        let stop_playback = Rc::clone(&stop_playback);
        let resume_raf = resume_raf.clone();
        Rc::new(move || {
            let queue = queue.clone();
            let player = Rc::clone(&player);
            let session = Rc::clone(&session);
            let fps_meter = Rc::clone(&fps_meter);
            let sync_ui = Rc::clone(&sync_ui);
            let stop_playback = Rc::clone(&stop_playback);
            let resume_raf = resume_raf.clone();
            // IMPORTANT: wasm_bindgen_futures::spawn_local (not leptos) — open may
            // be invoked from the file-input handler outside a reactive owner.
            spawn_local(async move {
                loop {
                    let Some((id, op)) = queue.pop_next() else {
                        if queue.try_end_drain() {
                            break;
                        }
                        continue;
                    };
                    if !queue.is_current(id) {
                        continue;
                    }
                    match op {
                        HostOp::Open { blob, canvas } => {
                            run_open(
                                &queue,
                                id,
                                blob,
                                canvas,
                                &player,
                                &sync_ui,
                                &fps_meter,
                                ready,
                                metadata,
                                error,
                                status,
                                fps,
                                pipeline,
                            )
                            .await;
                        }
                        HostOp::Seek(target) => {
                            run_seek_or_step(
                                &queue,
                                id,
                                &player,
                                &sync_ui,
                                &session,
                                error,
                                playing,
                                SeekOrStep::Seek(target),
                            )
                            .await;
                        }
                        HostOp::SeekKeyframe(target) => {
                            run_seek_or_step(
                                &queue,
                                id,
                                &player,
                                &sync_ui,
                                &session,
                                error,
                                playing,
                                SeekOrStep::SeekKeyframe(target),
                            )
                            .await;
                        }
                        HostOp::Scrub(target) => {
                            queue.set_scrub_inflight(true);
                            run_seek_or_step(
                                &queue,
                                id,
                                &player,
                                &sync_ui,
                                &session,
                                error,
                                playing,
                                SeekOrStep::Scrub(target),
                            )
                            .await;
                            queue.set_scrub_inflight(false);
                        }
                        HostOp::Step(forward) => {
                            run_seek_or_step(
                                &queue,
                                id,
                                &player,
                                &sync_ui,
                                &session,
                                error,
                                playing,
                                SeekOrStep::Step(forward),
                            )
                            .await;
                        }
                        HostOp::PlayPrime => {
                            run_play_prime(
                                &queue,
                                id,
                                &player,
                                &session,
                                &fps_meter,
                                &stop_playback,
                                &resume_raf,
                                ready,
                                playing,
                                error,
                                fps,
                            )
                            .await;
                        }
                    }
                }
            });
        }) as Rc<dyn Fn()>
    };

    let open_blob = {
        let queue = queue.clone();
        let stop_playback = Rc::clone(&stop_playback);
        let fps_meter = Rc::clone(&fps_meter);
        let kick_drain = Rc::clone(&kick_drain);
        Rc::new(move |blob: web_sys::Blob| {
            let Some(canvas_el) = canvas_ref.get_untracked() else {
                error.set(Some("canvas not mounted".into()));
                status.set("Canvas missing — refresh the page.".into());
                return;
            };
            let canvas: HtmlCanvasElement = canvas_el;
            // Always supersede in-flight work (mediabunny asyncId++ on new file).
            stop_playback();
            ready.set(false);
            error.set(None);
            fps.set(None);
            pipeline.set(None);
            metadata.set(None);
            let size_kb = (blob.size() as u64) / 1024;
            status.set(format!("Opening (~{size_kb} KB, progressive)…"));
            fps_meter.borrow_mut().reset();
            #[cfg(target_arch = "wasm32")]
            web_sys::console::log_1(
                &format!("webmedia: open_blob start ~{size_kb} KB").into(),
            );
            if queue.enqueue_open(blob, canvas) {
                kick_drain();
            }
        }) as Rc<dyn Fn(web_sys::Blob)>
    };

    let play_fn = {
        let player = Rc::clone(&player);
        let session = Rc::clone(&session);
        let queue = queue.clone();
        let kick_drain = Rc::clone(&kick_drain);
        Rc::new(move || {
            if !ready.get_untracked() {
                web_sys::console::log_1(&"webmedia: play ignored (not ready)".into());
                return;
            }
            // Stale `playing` with no session (Pause race) blocked all transport.
            if playing.get_untracked() {
                if session.borrow().is_some() {
                    web_sys::console::log_1(&"webmedia: play ignored (already playing)".into());
                    return;
                }
                web_sys::console::log_1(&"webmedia: recover stale playing flag".into());
                playing.set(false);
                if let Some(p) = player.borrow().as_ref().cloned() {
                    p.pause();
                }
            }
            if player.borrow().as_ref().is_none() {
                web_sys::console::log_1(&"webmedia: play ignored (no player)".into());
                return;
            }
            if queue.enqueue_play() {
                kick_drain();
            }
        }) as Rc<dyn Fn()>
    };

    let seek_fn = {
        let player = Rc::clone(&player);
        let session = Rc::clone(&session);
        let queue = queue.clone();
        let kick_drain = Rc::clone(&kick_drain);
        Rc::new(move |target: u32| {
            enqueue_seek_step(
                &queue,
                &kick_drain,
                &player,
                &session,
                playing,
                HostOp::Seek(target),
            );
        }) as Rc<dyn Fn(u32)>
    };

    let seek_keyframe_fn = {
        let player = Rc::clone(&player);
        let session = Rc::clone(&session);
        let queue = queue.clone();
        let kick_drain = Rc::clone(&kick_drain);
        Rc::new(move |target: u32| {
            enqueue_seek_step(
                &queue,
                &kick_drain,
                &player,
                &session,
                playing,
                HostOp::SeekKeyframe(target),
            );
        }) as Rc<dyn Fn(u32)>
    };

    let scrub_fn = {
        let player = Rc::clone(&player);
        let session = Rc::clone(&session);
        let queue = queue.clone();
        let kick_drain = Rc::clone(&kick_drain);
        Rc::new(move |target: u32| {
            enqueue_scrub(
                &queue,
                &kick_drain,
                &player,
                &session,
                playing,
                target,
            );
        }) as Rc<dyn Fn(u32)>
    };

    let next_fn = {
        let player = Rc::clone(&player);
        let session = Rc::clone(&session);
        let queue = queue.clone();
        let kick_drain = Rc::clone(&kick_drain);
        Rc::new(move || {
            enqueue_seek_step(
                &queue,
                &kick_drain,
                &player,
                &session,
                playing,
                HostOp::Step(true),
            );
        }) as Rc<dyn Fn()>
    };

    let prev_fn = {
        let player = Rc::clone(&player);
        let session = Rc::clone(&session);
        let queue = queue.clone();
        let kick_drain = Rc::clone(&kick_drain);
        Rc::new(move || {
            enqueue_seek_step(
                &queue,
                &kick_drain,
                &player,
                &session,
                playing,
                HostOp::Step(false),
            );
        }) as Rc<dyn Fn()>
    };

    let set_loop_fn = {
        let player = Rc::clone(&player);
        let sync_ui = Rc::clone(&sync_ui);
        Rc::new(move |on: bool| {
            if let Ok(borrow) = player.try_borrow() {
                if let Some(p) = borrow.as_ref() {
                    p.set_loop(on);
                }
            }
            sync_ui();
        }) as Rc<dyn Fn(bool)>
    };

    video.ops.set_value(Some(VideoOps {
        open_blob,
        play: play_fn,
        pause: stop_playback,
        seek_frame: seek_fn,
        seek_keyframe: seek_keyframe_fn,
        scrub_frame: scrub_fn,
        next_frame: next_fn,
        previous_frame: prev_fn,
        set_loop: set_loop_fn,
    }));

    on_cleanup({
        let ops = video.ops;
        move || ops.set_value(None)
    });

    let root_class = class.unwrap_or_else(|| "wm-stage".into());
    let viewport_class = viewport_class.unwrap_or_else(|| "wm-viewport".into());
    let canvas_class = canvas_class.unwrap_or_else(|| "wm-canvas".into());

    view! {
        <div class=root_class>
            <div class=viewport_class node_ref=viewport_ref>
                <canvas
                    node_ref=canvas_ref
                    class=canvas_class
                    width="320"
                    height="240"
                />
            </div>
        </div>
    }
}

fn enqueue_seek_step(
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

/// Async scrub drag — always cancel in-flight decode; latest frame wins.
fn enqueue_scrub(
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
    playing.set(false);
    let _ = session.borrow_mut().take();
    if let Some(p) = player.borrow().as_ref().cloned() {
        p.cancel_ops();
        p.pause();
    }
    if queue.enqueue_scrub(frame) {
        kick_drain();
    }
}

enum SeekOrStep {
    Seek(u32),
    SeekKeyframe(u32),
    Scrub(u32),
    Step(bool),
}

async fn run_open(
    queue: &OpQueue,
    id: u64,
    blob: web_sys::Blob,
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
) {
    web_sys::console::log_1(&"webmedia: open task running".into());
    if !queue.is_current(id) {
        return;
    }
    *player.borrow_mut() = None;
    let opened = open_blob_staged(blob, canvas, {
        let status = status;
        move |msg: &str| {
            web_sys::console::log_1(&format!("webmedia: status {msg}").into());
            status.set(msg.to_string());
        }
    })
    .await;
    if !queue.is_current(id) {
        return;
    }
    match opened {
        Ok(p) => {
            let p = Rc::new(p);
            metadata.set(Some(p.metadata()));
            let _ = p.relayout();
            *player.borrow_mut() = Some(Rc::clone(&p));
            status.set("Decoding frame 0…".into());
            let seek = p.seek_frame(0).await;
            if !queue.is_current(id) {
                return;
            }
            match seek {
                Ok(()) => {
                    ready.set(true);
                    status.set("Ready.".into());
                    fps_meter.borrow_mut().reset();
                    fps.set(None);
                    pipeline.set(Some(p.pipeline_stats()));
                    sync_ui();
                }
                Err(e) if e.is_cancelled() => {
                    status.set("Open cancelled.".into());
                }
                Err(e) => {
                    ready.set(false);
                    error.set(Some(e.to_string()));
                    status.set(format!("Seek 0 failed: {e}"));
                }
            }
        }
        Err(e) => {
            ready.set(false);
            metadata.set(None);
            error.set(Some(e.clone()));
            status.set(format!("Open failed: {e}"));
            web_sys::console::error_1(&e.clone().into());
        }
    }
}

async fn run_seek_or_step(
    queue: &OpQueue,
    id: u64,
    player: &PlayerHandle,
    sync_ui: &Rc<dyn Fn()>,
    session: &Rc<RefCell<Option<PlaySession>>>,
    error: RwSignal<Option<String>>,
    playing: RwSignal<bool>,
    op: SeekOrStep,
) {
    if !queue.is_current(id) {
        return;
    }
    let Some(p) = player.borrow().as_ref().cloned() else {
        return;
    };
    let _ = session.borrow_mut().take();
    playing.set(false);
    p.pause();
    let label = match &op {
        SeekOrStep::Seek(t) => format!("seek {t}"),
        SeekOrStep::SeekKeyframe(t) => format!("seek-key {t}"),
        SeekOrStep::Scrub(t) => format!("scrub {t}"),
        SeekOrStep::Step(true) => "next".into(),
        SeekOrStep::Step(false) => "prev".into(),
    };
    web_sys::console::log_1(&format!("webmedia: {label} start").into());
    let result = match op {
        SeekOrStep::Seek(target) => p.seek_frame(u64::from(target)).await,
        SeekOrStep::SeekKeyframe(target) => p.seek_keyframe(u64::from(target)).await,
        SeekOrStep::Scrub(target) => p.scrub_to(u64::from(target)).await,
        SeekOrStep::Step(true) => p.next_frame().await,
        SeekOrStep::Step(false) => p.previous_frame().await,
    };
    if !queue.is_current(id) {
        return;
    }
    web_sys::console::log_1(
        &format!(
            "webmedia: {label} done {}",
            if result.is_ok() { "ok" } else { "err" }
        )
        .into(),
    );
    if let Err(e) = &result {
        if !e.is_cancelled() {
            error.set(Some(e.to_string()));
        }
    }
    sync_ui();
    playing.set(false);
}

async fn run_play_prime(
    queue: &OpQueue,
    id: u64,
    player: &PlayerHandle,
    session: &Rc<RefCell<Option<PlaySession>>>,
    fps_meter: &Rc<RefCell<FpsMeter>>,
    stop_playback: &Rc<dyn Fn()>,
    resume_raf: &impl Fn(),
    ready: RwSignal<bool>,
    playing: RwSignal<bool>,
    error: RwSignal<Option<String>>,
    fps: RwSignal<Option<f64>>,
) {
    if !queue.is_current(id) || !ready.get_untracked() {
        return;
    }
    let Some(p) = player.borrow().as_ref().cloned() else {
        return;
    };

    p.play();
    fps_meter.borrow_mut().reset();
    fps.set(None);
    playing.set(true);

    let still_mine = || queue.is_current(id) && playing.get_untracked();

    web_sys::console::log_1(&"webmedia: play prime start".into());
    let prime_ok = match p.prime_playback().await {
        Ok(()) => true,
        Err(e) if e.is_cancelled() => false,
        Err(e) => {
            if still_mine() {
                error.set(Some(e.to_string()));
            }
            false
        }
    };
    web_sys::console::log_1(&format!("webmedia: play prime done ok={prime_ok}").into());

    if !still_mine() {
        return;
    }
    if !prime_ok {
        stop_playback();
        return;
    }

    let media_origin = p.state().timestamp.seconds;
    let wall_origin = performance_now_ms().map(|t| t / 1000.0).unwrap_or(0.0);
    let last_ui_ms = performance_now_ms().unwrap_or(0.0);

    *session.borrow_mut() = Some(PlaySession {
        media_origin,
        wall_origin,
        last_ui_ms,
        restarting: false,
    });

    {
        let player = Rc::clone(player);
        let stop_playback = Rc::clone(stop_playback);
        let queue = queue.clone();
        spawn_local(async move {
            while playing.get_untracked() && queue.is_current(id) {
                let need = {
                    let borrow = player.borrow();
                    borrow.as_ref().is_some_and(|p| {
                        p.is_playing()
                            && !p.is_restarting()
                            && p.presentation_len() < webmedia::PRESENTATION_QUEUE_CAP
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

    if still_mine() {
        resume_raf();
    }
}

async fn open_blob_staged(
    blob: web_sys::Blob,
    canvas: HtmlCanvasElement,
    on_status: impl Fn(&str),
) -> Result<WebPlayer, String> {
    on_status("Starting demux…");
    let source = std::sync::Arc::new(BlobSource::new(blob));
    web_sys::console::log_1(&"webmedia: demux start".into());
    let track = open_mp4_source_cached_with_progress(
        source,
        OpenMp4Options {
            // Mediabunny verifyKeyPackets — Wing It / Resolve clips mark non-IDR
            // samples in `stss`; skip-behind then jumps to a fake key and dies.
            verify_avc_sync: true,
        },
        |msg| on_status(msg),
    )
    .await
    .map_err(|e| e.to_string())?;
    web_sys::console::log_1(
        &format!("webmedia: demux ok, {} frames", track.frame_count()).into(),
    );
    on_status(&format!(
        "moov ok — {} frames, creating decoder…",
        track.frame_count()
    ));
    WebPlayer::from_track(track, canvas)
        .await
        .map_err(|e| e.to_string())
}

/// Await one animation frame with a dedicated Closure (not Promise.resolve).
async fn wait_animation_frame() {
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

fn performance_now_ms() -> Option<f64> {
    web_sys::window()?.performance().map(|p| p.now())
}
