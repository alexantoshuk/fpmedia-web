//! [`Video`] component — canvas stage + playback orchestration.

use std::cell::RefCell;
use std::rc::Rc;

use leptos::html::{Canvas, Div};
use leptos::prelude::*;
use leptos_use::utils::Pausable;
use leptos_use::{
    ThrottleOptions, UseRafFnOptions, use_raf_fn_with_options, use_resize_observer,
    use_throttle_fn_with_arg_and_options,
};
use wasm_bindgen::JsCast;
use wasm_bindgen::closure::Closure;
use wasm_bindgen_futures::{JsFuture, spawn_local};
use web_sys::{HtmlCanvasElement, Response};
use webmedia::{
    BlobSource, DuePresent, MediaError, MediaSource, OpenMp4Options, UrlSource, WebPlayer,
    is_no_http_range, open_mp4_media_source_cached_with_progress,
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
    /// Media clock frozen while presentation queue is empty (underrun hold).
    holding_for_buffer: bool,
    /// `start_media_clock` after hold in flight — do not stack resumes.
    resuming_clock: bool,
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
    let ended = video.ended;
    let frame = video.frame;
    let playhead = video.playhead;
    let frame_count = video.frame_count;
    let timestamp = video.timestamp;
    let fps = video.fps;
    let metadata = video.metadata;
    let error = video.error;
    let pipeline = video.pipeline;
    let status = video.status;
    let has_audio = video.has_audio;
    let volume = video.volume;
    let muted = video.muted;
    let playback_rate = video.playback_rate;

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
                let engine = st.frame.min(u32::MAX as u64) as u32;
                let count = st.frame_count.min(u32::MAX as u64) as u32;
                let count_changed = frame_count.get_untracked() != count;
                frame.set(engine);
                frame_count.set(count);
                timestamp.set(st.timestamp.seconds);
                // Follow engine while playing, or reset playhead on a new clip.
                // While paused after scrub, leave `playhead` (UI target) alone.
                if playing.get_untracked() || count_changed {
                    playhead.set(engine);
                }
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
            let leaving_play = playing.get_untracked();
            playing.set(false);
            let _ = session.borrow_mut().take();
            let p = player.borrow().as_ref().cloned();
            if let Some(p) = p {
                p.pause();
            }
            fps_meter.borrow_mut().reset();
            fps.set(None);
            sync_ui();
            // `sync_ui` skips playhead while paused (preserve scrub targets). Leaving
            // play must still align playhead to the committed canvas/engine frame —
            // otherwise Pause→Seek/Prev/Next use a ~100ms-stale playhead.
            if leaving_play {
                playhead.set(frame.get_untracked());
            }
            playing.set(false);
            // `busy` stays with the op queue (open/seek may still be draining).
        }) as Rc<dyn Fn()>
    };

    let Pausable {
        pause: _pause_raf,
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
                // Play path: normal present/prefetch UI.
                // Pause path: still tick so gap-fill strip marks update (fill is async).
                if !playing.get_untracked() {
                    let Ok(borrow) = player.try_borrow() else {
                        return;
                    };
                    if let Some(p) = borrow.as_ref() {
                        // Refresh while filling and once more after busy clears.
                        pipeline.set(Some(p.pipeline_stats()));
                    }
                    return;
                }

                // Session is armed only after prime + clock start. Skip present while
                // priming / if a stale `playing` flag has no session — otherwise
                // `DuePresent::Ended` → `stop_playback` can bump async_id mid-PlayPrime.
                let restarting = {
                    let borrow = session.borrow();
                    let Some(s) = borrow.as_ref() else {
                        return;
                    };
                    s.restarting
                };
                if restarting {
                    return;
                }

                {
                    let want = looping.get_untracked();
                    if let Ok(borrow) = player.try_borrow()
                        && let Some(p) = borrow.as_ref()
                    {
                        p.set_loop(want);
                    }
                }

                let t = {
                    let borrow = player.borrow();
                    match borrow.as_ref() {
                        Some(p) => p.media_time(),
                        None => {
                            stop_playback();
                            return;
                        }
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
                        ended.set(true);
                        stop_playback();
                        status.set("Ended.".into());
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
                                        s.wall_origin =
                                            performance_now_ms().map(|n| n / 1000.0).unwrap_or(0.0);
                                        s.restarting = false;
                                    }
                                    // Warm audio restart — loop_prime already painted
                                    // video; hard `start_media_clock` tore down the
                                    // AudioDecoder and felt like audio-only lag.
                                    let _ = p.resume_media_clock(0.0).await;
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
                        if did_draw && let Some(measured) = fps_meter.borrow_mut().note_present() {
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
                        if let Some(now) = performance_now_ms() {
                            let mut borrow = session.borrow_mut();
                            if let Some(s) = borrow.as_mut()
                                && now - s.last_ui_ms >= 100.0
                            {
                                s.last_ui_ms = now;
                                drop(borrow);
                                sync_ui();
                            }
                        }
                        // True underrun only: next frame already due + empty queue.
                        // Holding on every empty-between-presents thrashed audio and
                        // broke short-clip play/pause smoke.
                        let underrun = match player.try_borrow() {
                            Ok(borrow) => borrow
                                .as_ref()
                                .is_some_and(|p| p.is_playing() && p.play_underrun_hold()),
                            Err(_) => false,
                        };
                        if underrun {
                            let should_hold = {
                                let mut borrow = session.borrow_mut();
                                borrow.as_mut().is_some_and(|s| {
                                    if s.holding_for_buffer || s.resuming_clock || s.restarting {
                                        false
                                    } else {
                                        s.holding_for_buffer = true;
                                        true
                                    }
                                })
                            };
                            if should_hold
                                && let Ok(borrow) = player.try_borrow()
                                && let Some(p) = borrow.as_ref()
                            {
                                let t = p.hold_media_clock_for_underrun();
                                if let Some(s) = session.borrow_mut().as_mut() {
                                    s.media_origin = t;
                                }
                            }
                        } else {
                            let should_resume = {
                                let has_frames = player
                                    .try_borrow()
                                    .ok()
                                    .and_then(|b| b.as_ref().map(|p| p.presentation_len() > 0))
                                    .unwrap_or(false);
                                let mut borrow = session.borrow_mut();
                                borrow.as_mut().is_some_and(|s| {
                                    if s.holding_for_buffer
                                        && !s.resuming_clock
                                        && !s.restarting
                                        && has_frames
                                    {
                                        s.holding_for_buffer = false;
                                        s.resuming_clock = true;
                                        true
                                    } else {
                                        false
                                    }
                                })
                            };
                            if should_resume {
                                let player = Rc::clone(&player);
                                let session = Rc::clone(&session);
                                let stop_playback = Rc::clone(&stop_playback);
                                spawn_local(async move {
                                    let Some(p) = player.borrow().as_ref().cloned() else {
                                        if let Some(s) = session.borrow_mut().as_mut() {
                                            s.resuming_clock = false;
                                        }
                                        return;
                                    };
                                    let t = session
                                        .borrow()
                                        .as_ref()
                                        .map(|s| s.media_origin)
                                        .unwrap_or(0.0);
                                    match p.resume_media_clock(t).await {
                                        Ok(()) => {
                                            if let Some(s) = session.borrow_mut().as_mut() {
                                                s.resuming_clock = false;
                                            }
                                        }
                                        Err(e) if e.is_cancelled() => {
                                            if let Some(s) = session.borrow_mut().as_mut() {
                                                s.resuming_clock = false;
                                            }
                                        }
                                        Err(e) => {
                                            if let Some(s) = session.borrow_mut().as_mut() {
                                                s.resuming_clock = false;
                                            }
                                            error.set(Some(e.to_string()));
                                            stop_playback();
                                        }
                                    }
                                });
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
        let resume_raf = resume_raf.clone();
        move |_| {
            // Keep rAF alive while paused so gap-fill can refresh the strip
            // (`scrub_busy` ticks in the callback above).
            let _ = playing.get();
            resume_raf();
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
                        HostOp::Open { source, canvas } => {
                            run_open(
                                &queue,
                                id,
                                source,
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
                                has_audio,
                                volume,
                                muted,
                                playback_rate,
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
                                playhead,
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
                                playhead,
                                SeekOrStep::SeekKeyframe(target),
                            )
                            .await;
                        }
                        HostOp::Scrub {
                            frame: target,
                            settle,
                        } => {
                            queue.set_scrub_inflight(true);
                            run_seek_or_step(
                                &queue,
                                id,
                                &player,
                                &sync_ui,
                                &session,
                                error,
                                playing,
                                playhead,
                                SeekOrStep::Scrub { target, settle },
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
                                playhead,
                                SeekOrStep::Step(forward),
                            )
                            .await;
                            // Cached steps can finish in one microtask; without a
                            // paint yield the drain chains several frames before
                            // the browser composites → lag then a burst.
                            wait_animation_frame().await;
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
                                playhead,
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
            ended.set(false);
            error.set(None);
            fps.set(None);
            pipeline.set(None);
            metadata.set(None);
            frame.set(0);
            playhead.set(0);
            frame_count.set(0);
            timestamp.set(0.0);
            has_audio.set(false);
            let size_kb = (blob.size() as u64) / 1024;
            status.set(format!("Opening (~{size_kb} KB, progressive)…"));
            fps_meter.borrow_mut().reset();
            #[cfg(target_arch = "wasm32")]
            web_sys::console::log_1(&format!("webmedia: open_blob start ~{size_kb} KB").into());
            let source: std::sync::Arc<dyn MediaSource> =
                std::sync::Arc::new(BlobSource::new(blob));
            if queue.enqueue_open(source, canvas) {
                kick_drain();
            }
        }) as Rc<dyn Fn(web_sys::Blob)>
    };

    let open_url = {
        let queue = queue.clone();
        let stop_playback = Rc::clone(&stop_playback);
        let fps_meter = Rc::clone(&fps_meter);
        let kick_drain = Rc::clone(&kick_drain);
        Rc::new(move |url: String| {
            let Some(canvas_el) = canvas_ref.get_untracked() else {
                error.set(Some("canvas not mounted".into()));
                status.set("Canvas missing — refresh the page.".into());
                return;
            };
            let canvas: HtmlCanvasElement = canvas_el;
            let url = url.trim().to_string();
            if url.is_empty() {
                error.set(Some("empty URL".into()));
                status.set("Enter a URL to open.".into());
                return;
            }
            stop_playback();
            queue.cancel_all();
            let fetch_id = queue.current_id();
            ready.set(false);
            ended.set(false);
            error.set(None);
            fps.set(None);
            pipeline.set(None);
            metadata.set(None);
            frame.set(0);
            playhead.set(0);
            frame_count.set(0);
            timestamp.set(0.0);
            has_audio.set(false);
            fps_meter.borrow_mut().reset();
            busy.set(true);
            // Same-origin /proxy bypasses CDN CORS (GitHub Releases, S3, …).
            let url = same_origin_media_proxy_url(&url).unwrap_or(url);
            status.set(format!("Opening {url}…"));
            #[cfg(target_arch = "wasm32")]
            web_sys::console::log_1(&format!("webmedia: open_url start {url}").into());

            let queue = queue.clone();
            let kick_drain = Rc::clone(&kick_drain);
            spawn_local(async move {
                let opened = resolve_url_source(&url, |msg| status.set(msg.to_string())).await;
                if !queue.is_current(fetch_id) {
                    return;
                }
                match opened {
                    Ok(source) => {
                        if queue.enqueue_open(source, canvas) {
                            kick_drain();
                        }
                    }
                    Err(e) => {
                        busy.set(false);
                        ready.set(false);
                        error.set(Some(e.clone()));
                        status.set("Open URL failed.".into());
                        #[cfg(target_arch = "wasm32")]
                        web_sys::console::error_1(&e.into());
                    }
                }
            });
        }) as Rc<dyn Fn(String)>
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

    // Rate-limit scrub drag only — pointermove is denser than key-repeat.
    // Next/Prev: OpQueue coalesce + hold-safe playhead aim (no throttle).
    // Settle (pointer up) stays immediate — exact must not wait for a throttle window.
    const SCRUB_DRAG_THROTTLE_MS: f64 = 50.0;

    let scrub_drag_throttled = {
        let player = Rc::clone(&player);
        let session = Rc::clone(&session);
        let queue = queue.clone();
        let kick_drain = Rc::clone(&kick_drain);
        use_throttle_fn_with_arg_and_options(
            move |target: u32| {
                enqueue_scrub(
                    &queue,
                    &kick_drain,
                    &player,
                    &session,
                    playing,
                    target,
                    false,
                );
            },
            SCRUB_DRAG_THROTTLE_MS,
            // trailing=false: a post-pointerup trailing drag can enqueue after
            // settle + PlayPrime and silently kill play (EOF→scrub→Play smoke).
            // Settle already exacts the live playhead; trailing is redundant.
            ThrottleOptions::default().leading(true).trailing(false),
        )
    };

    let scrub_fn = {
        let player = Rc::clone(&player);
        let session = Rc::clone(&session);
        let queue = queue.clone();
        let kick_drain = Rc::clone(&kick_drain);
        let scrub_drag_throttled = scrub_drag_throttled.clone();
        Rc::new(move |target: u32, settle: bool| {
            if settle {
                enqueue_scrub(
                    &queue,
                    &kick_drain,
                    &player,
                    &session,
                    playing,
                    target,
                    true,
                );
            } else {
                scrub_drag_throttled(target);
            }
        }) as Rc<dyn Fn(u32, bool)>
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
            if let Ok(borrow) = player.try_borrow()
                && let Some(p) = borrow.as_ref()
            {
                p.set_loop(on);
            }
            sync_ui();
        }) as Rc<dyn Fn(bool)>
    };

    let set_volume_fn = {
        let player = Rc::clone(&player);
        Rc::new(move |v: f64| {
            if let Ok(borrow) = player.try_borrow()
                && let Some(p) = borrow.as_ref()
            {
                p.set_volume(v);
            }
        }) as Rc<dyn Fn(f64)>
    };

    let set_muted_fn = {
        let player = Rc::clone(&player);
        Rc::new(move |m: bool| {
            if let Ok(borrow) = player.try_borrow()
                && let Some(p) = borrow.as_ref()
            {
                p.set_muted(m);
            }
        }) as Rc<dyn Fn(bool)>
    };

    let set_playback_rate_fn = {
        let player = Rc::clone(&player);
        Rc::new(move |r: f64| {
            if let Ok(borrow) = player.try_borrow()
                && let Some(p) = borrow.as_ref()
            {
                p.set_playback_rate(r);
            }
        }) as Rc<dyn Fn(f64)>
    };

    let close_fn = {
        let player = Rc::clone(&player);
        let session = Rc::clone(&session);
        let fps_meter = Rc::clone(&fps_meter);
        let queue = queue.clone();
        Rc::new(move || {
            queue.cancel_all();
            playing.set(false);
            let _ = session.borrow_mut().take();
            // Drop WebPlayer (decoder + caches); cancel_ops via Drop.
            let retiring = player.borrow_mut().take();
            if let Some(p) = retiring {
                p.pause();
            }
            fps_meter.borrow_mut().reset();
            ready.set(false);
            ended.set(false);
            frame.set(0);
            playhead.set(0);
            frame_count.set(0);
            timestamp.set(0.0);
            fps.set(None);
            metadata.set(None);
            pipeline.set(None);
            has_audio.set(false);
            error.set(None);
            status.set("Closed.".into());
        }) as Rc<dyn Fn()>
    };

    video.ops.set_value(Some(VideoOps {
        open_blob,
        open_url,
        play: play_fn,
        pause: stop_playback,
        close: Rc::clone(&close_fn),
        seek_frame: seek_fn,
        seek_keyframe: seek_keyframe_fn,
        scrub_frame: scrub_fn,
        next_frame: next_fn,
        previous_frame: prev_fn,
        set_loop: set_loop_fn,
        set_volume: set_volume_fn,
        set_muted: set_muted_fn,
        set_playback_rate: set_playback_rate_fn,
    }));

    on_cleanup({
        let ops = video.ops;
        move || {
            let close = ops
                .try_with_value(|o| o.as_ref().map(|v| Rc::clone(&v.close)))
                .flatten();
            if let Some(close) = close {
                close();
            }
            ops.set_value(None);
        }
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
    settle: bool,
) {
    if player.borrow().as_ref().is_none() {
        return;
    }
    // Leaving play: full pause once. Already paused: do **not** call `WebPlayer::pause`
    // on every pointer sample (re-enters scrub pipeline / parks queue and can nest
    // into Leptos event handlers → tachys `RefCell already borrowed`).
    let leaving_play = playing.get_untracked();
    if leaving_play {
        playing.set(false);
        let _ = session.borrow_mut().take();
    }
    if let Some(p) = player.borrow().as_ref().cloned() {
        if settle {
            // Abort densify + hold HW for exact *before* the async op runs.
            // Do not `note_scrub_frame` — that kicks densify and starved settle.
            p.prepare_scrub_settle(u64::from(frame));
        } else {
            p.clear_densify_hold();
            p.cancel_ops();
            if leaving_play {
                p.pause();
            }
            p.note_scrub_frame(u64::from(frame));
        }
    }
    if queue.enqueue_scrub(frame, settle) {
        kick_drain();
    }
}

enum SeekOrStep {
    Seek(u32),
    SeekKeyframe(u32),
    Scrub { target: u32, settle: bool },
    Step(bool),
}

#[allow(clippy::too_many_arguments)] // Leptos open task wires many signals.
async fn run_open(
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
    let opened = open_source_staged(source, canvas, move |msg: &str| {
        web_sys::console::log_1(&format!("webmedia: status {msg}").into());
        status.set(msg.to_string());
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
                    status.set("Seek 0 failed.".into());
                }
            }
        }
        Err(e) => {
            ready.set(false);
            metadata.set(None);
            error.set(Some(e.clone()));
            status.set("Open failed.".into());
            web_sys::console::error_1(&e.clone().into());
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_seek_or_step(
    queue: &OpQueue,
    id: u64,
    player: &PlayerHandle,
    sync_ui: &Rc<dyn Fn()>,
    session: &Rc<RefCell<Option<PlaySession>>>,
    error: RwSignal<Option<String>>,
    playing: RwSignal<bool>,
    playhead: RwSignal<u32>,
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
        SeekOrStep::Scrub { target: t, settle } => {
            if *settle {
                format!("scrub settle {t}")
            } else {
                format!("scrub {t}")
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
        SeekOrStep::Step(true) => p.next_frame().await,
        SeekOrStep::Step(false) => p.previous_frame().await,
    };
    if !queue.is_current(id) {
        return;
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
}

#[allow(clippy::too_many_arguments)]
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
    playhead: RwSignal<u32>,
    error: RwSignal<Option<String>>,
    fps: RwSignal<Option<f64>>,
) {
    if !queue.is_current(id) || !ready.get_untracked() {
        return;
    }
    let Some(p) = player.borrow().as_ref().cloned() else {
        return;
    };

    // Sync engine to UI playhead only if still mismatched (e.g. settle cancelled
    // by seek/step, or play without a prior settle). Do this *before* `play()` so
    // we do not cancel an in-flight scrub that was already targeting playhead —
    // that op finished ahead of us in the drain queue.
    let ph = playhead.get_untracked();
    if p.state().frame != u64::from(ph) {
        web_sys::console::log_1(
            &format!("webmedia: play sync seek {ph} (engine {})", p.state().frame).into(),
        );
        match p.seek_frame(u64::from(ph)).await {
            Ok(()) => {}
            Err(e) if e.is_cancelled() => return,
            Err(e) => {
                error.set(Some(e.to_string()));
                return;
            }
        }
        if !queue.is_current(id) {
            return;
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
        return;
    }

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
        return;
    }
    if !prime_ok {
        stop_playback();
        return;
    }

    let media_origin = p.state().timestamp.seconds;
    if let Err(e) = p.start_media_clock(media_origin).await {
        if still_queued() {
            error.set(Some(e.to_string()));
            stop_playback();
        }
        return;
    }
    if !still_queued() {
        if p.is_playing() {
            p.pause();
        }
        return;
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
}

/// Resolve `https:` via [`UrlSource`] (HTTP Range); `blob:` / no-Range hosts via
/// full fetch → [`BlobSource`]. Status line only — no confirm dialogs.
///
/// Remote `http(s):` URLs are rewritten to same-origin `/proxy?url=…` when the
/// page has a `window` (demo Axum shell) so GitHub Release / S3 CORS does not
/// block Range GETs.
fn same_origin_media_proxy_url(remote: &str) -> Option<String> {
    let trimmed = remote.trim();
    let lower = trimmed.to_ascii_lowercase();
    if !(lower.starts_with("https:") || lower.starts_with("http:")) {
        return None;
    }
    if trimmed.contains("/proxy?") {
        return None;
    }
    let window = web_sys::window()?;
    let origin = window.location().origin().ok()?;
    if trimmed.starts_with(&origin) {
        return None;
    }
    let enc = String::from(js_sys::encode_uri_component(trimmed));
    Some(format!("{origin}/proxy?url={enc}"))
}

async fn resolve_url_source(
    url: &str,
    on_status: impl Fn(&str),
) -> Result<std::sync::Arc<dyn MediaSource>, String> {
    let lower = url.to_ascii_lowercase();
    if lower.starts_with("blob:") {
        on_status("Opening blob…");
        let blob = fetch_url_as_blob(url).await?;
        return Ok(std::sync::Arc::new(BlobSource::new(blob)));
    }
    if !(lower.starts_with("https:") || lower.starts_with("http:")) {
        return Err(format!(
            "unsupported URL scheme (need https:, http:, or blob:): {url}"
        ));
    }

    on_status("Probing HTTP Range…");
    match UrlSource::connect(url.to_string()).await {
        Ok(src) => {
            let size_kb = src.size() / 1024;
            web_sys::console::log_1(
                &format!("webmedia: open_url UrlSource size={size_kb} KB").into(),
            );
            on_status(&format!("Opening (progressive, ~{size_kb} KB)…"));
            Ok(std::sync::Arc::new(src))
        }
        Err(e) if is_no_http_range(&e) => {
            // mediabunny: src/source.ts UrlSource — non-206 → sequential/full path;
            // we fall back to a whole-resource fetch into BlobSource.
            web_sys::console::warn_1(
                &format!("webmedia: no HTTP Range ({e}); full download fallback").into(),
            );
            let size_hint = content_length_from_no_range(&e)
                .map(format_byte_size)
                .map(|s| format!(" (~{s})"))
                .unwrap_or_default();
            on_status(&format!(
                "No HTTP Range — downloading full file{size_hint}…"
            ));
            let blob = fetch_url_as_blob(url).await?;
            let size_kb = (blob.size() as u64) / 1024;
            web_sys::console::log_1(&format!("webmedia: open_url full fetch ~{size_kb} KB").into());
            on_status(&format!(
                "Opening (full download, ~{})…",
                format_byte_size(blob.size() as u64)
            ));
            Ok(std::sync::Arc::new(BlobSource::new(blob)))
        }
        Err(e) => Err(e.to_string()),
    }
}

/// Parse `content-length=N` from [`is_no_http_range`] error text when present.
fn content_length_from_no_range(err: &MediaError) -> Option<u64> {
    let msg = err.to_string();
    let rest = msg.split("content-length=").nth(1)?;
    rest.split(|c: char| !c.is_ascii_digit())
        .next()?
        .parse()
        .ok()
}

fn format_byte_size(bytes: u64) -> String {
    if bytes >= 1024 * 1024 {
        format!("{:.0} MB", bytes as f64 / (1024.0 * 1024.0))
    } else if bytes >= 1024 {
        format!("{} KB", bytes / 1024)
    } else {
        format!("{bytes} B")
    }
}

/// Full-resource fetch → [`web_sys::Blob`] (`blob:` / no-Range fallback).
async fn fetch_url_as_blob(url: &str) -> Result<web_sys::Blob, String> {
    let window = web_sys::window().ok_or_else(|| "no window".to_string())?;
    let resp_val = JsFuture::from(window.fetch_with_str(url))
        .await
        .map_err(|e| format_js_error("fetch", &e))?;
    let resp: Response = resp_val
        .dyn_into()
        .map_err(|_| "fetch did not return a Response".to_string())?;
    if !resp.ok() {
        return Err(format!("HTTP {}", resp.status()));
    }
    let blob_promise = resp
        .blob()
        .map_err(|e| format_js_error("response.blob", &e))?;
    let blob_val = JsFuture::from(blob_promise)
        .await
        .map_err(|e| format_js_error("blob", &e))?;
    blob_val
        .dyn_into::<web_sys::Blob>()
        .map_err(|_| "response body is not a Blob".to_string())
}

fn format_js_error(what: &str, err: &wasm_bindgen::JsValue) -> String {
    if let Some(s) = err.as_string() {
        return format!("{what}: {s}");
    }
    format!("{what}: {err:?}")
}

async fn open_source_staged(
    source: std::sync::Arc<dyn MediaSource>,
    canvas: HtmlCanvasElement,
    on_status: impl Fn(&str),
) -> Result<WebPlayer, String> {
    on_status("Starting demux…");
    web_sys::console::log_1(&"webmedia: demux start".into());
    let opened = open_mp4_media_source_cached_with_progress(
        source,
        OpenMp4Options {
            // Mediabunny verifyKeyPackets — demote lying stss (non-IDR / inexact
            // SEI) so skip-behind does not jump to a fake key and die.
            verify_avc_sync: true,
        },
        |msg| on_status(msg),
    )
    .await
    .map_err(|e| e.to_string())?;
    let video = opened.video.ok_or_else(|| "no video track".to_string())?;
    let audio = opened.audio;
    web_sys::console::log_1(
        &format!(
            "webmedia: demux ok, {} frames, audio={}",
            video.frame_count(),
            audio.is_some()
        )
        .into(),
    );
    on_status(&format!(
        "moov ok — {} frames{}, creating decoder…",
        video.frame_count(),
        if audio.is_some() { " + audio" } else { "" }
    ));
    WebPlayer::from_tracks_with_fetch(video, audio, canvas, opened.fetch_map)
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
