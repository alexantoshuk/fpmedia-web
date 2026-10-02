//! [`Video`] component — canvas stage + playback orchestration.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use leptos::html::{Canvas, Div};
use leptos::prelude::*;
use leptos_use::utils::Pausable;
use leptos_use::{
    ThrottleOptions, UseRafFnOptions, use_event_listener, use_raf_fn_with_options,
    use_resize_observer, use_throttle_fn_with_arg_and_options,
};
use wasm_bindgen_futures::spawn_local;
use web_sys::HtmlCanvasElement;
use webmedia::{BlobSource, DuePresent, MediaSource, OpenPhase, OpenSourceKind, TransportEvent};

use super::host_ops::{
    PlayerHandle, SeekOrStep, enqueue_seek_step, run_open, run_play_prime, run_seek_or_step,
    wait_animation_frame,
};
use super::op_queue::{HostOp, OpQueue};
use super::open_url::{resolve_url_source, same_origin_media_proxy_url};
use super::session::{FpsMeter, PlaySession, performance_now_ms};
use super::transport_apply::{dispatch_transport, new_transport, note_transport};
use super::{VideoOps, VideoRef};

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
    let transport = new_transport();

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
                // fMP4 bg backfill grows the index after warm open — refresh meta.
                if count_changed {
                    metadata.set(Some(p.metadata()));
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
            // Abort in-flight play-prime / scrub-release / prefetch.
            queue.bump();
            queue.clear_pending_scrub_and_play();
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

    // Tab / window hidden while playing: freeze clock + soft-hold audio so
    // restore does not skip-catch with silent AudioContext.
    // mediabunny media-player: no visibility handler — local adaptation.
    let visibility_held = Rc::new(Cell::new(false));
    let _visibility = {
        let player = Rc::clone(&player);
        let session = Rc::clone(&session);
        let visibility_held = Rc::clone(&visibility_held);
        let document = web_sys::window()
            .and_then(|w| w.document())
            .expect("document");
        use_event_listener(document.clone(), leptos::ev::visibilitychange, move |_| {
            if document.hidden() {
                if !playing.get_untracked() || session.borrow().is_none() {
                    return;
                }
                let Ok(borrow) = player.try_borrow() else {
                    return;
                };
                let Some(p) = borrow.as_ref() else {
                    return;
                };
                if let Some(t) = p.hold_media_clock_for_hidden() {
                    drop(borrow);
                    if let Some(s) = session.borrow_mut().as_mut() {
                        s.media_origin = t;
                        s.holding_for_buffer = false;
                        s.resuming_clock = false;
                    }
                    visibility_held.set(true);
                    web_sys::console::log_1(&"webmedia: visibility hold".into());
                }
                return;
            }
            if !visibility_held.get() {
                return;
            }
            visibility_held.set(false);
            if !playing.get_untracked() || session.borrow().is_none() {
                return;
            }
            let player = Rc::clone(&player);
            let session = Rc::clone(&session);
            spawn_local(async move {
                let Some(p) = player.borrow().as_ref().cloned() else {
                    return;
                };
                if let Err(e) = p.resume_audio_context().await {
                    web_sys::console::warn_1(
                        &format!("webmedia: visibility audio resume: {e}").into(),
                    );
                }
                let t = session
                    .borrow()
                    .as_ref()
                    .map(|s| s.media_origin)
                    .unwrap_or_else(|| p.state().timestamp.seconds);
                match p.resume_media_clock(t).await {
                    Ok(()) => {
                        web_sys::console::log_1(&"webmedia: visibility resume".into());
                    }
                    Err(e) => {
                        web_sys::console::warn_1(
                            &format!("webmedia: visibility clock resume: {e}").into(),
                        );
                    }
                }
            });
        })
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
            let transport = transport.clone();
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
                        // Fragment backfill / deferred sync may grow frame_count while
                        // paused — sync_ui only overwrites playhead when count changes.
                        sync_ui();
                    }
                    return;
                }

                // Session is armed only after prime (+ deferred silent clock on
                // first tick). Skip present while priming / if a stale `playing`
                // flag has no session — otherwise `DuePresent::Ended` →
                // `stop_playback` can bump async_id mid-PlayPrime.
                let (restarting, pending_clock, media_origin) = {
                    let borrow = session.borrow();
                    let Some(s) = borrow.as_ref() else {
                        return;
                    };
                    (s.restarting, s.pending_clock_start, s.media_origin)
                };
                if restarting {
                    return;
                }

                // No-AAC: paint presentation front then arm PlaybackClock / wall
                // (mediabunny: startVideoIterator draw → play / audioContextStartTime).
                if pending_clock {
                    if let Some(s) = session.borrow_mut().as_mut() {
                        s.pending_clock_start = false;
                    }
                    if let Ok(borrow) = player.try_borrow()
                        && let Some(p) = borrow.as_ref()
                    {
                        match p.finish_silent_play_prime() {
                            Ok(origin) => {
                                if let Some(s) = session.borrow_mut().as_mut() {
                                    s.media_origin = origin;
                                }
                            }
                            Err(e) => {
                                web_sys::console::warn_1(
                                    &format!("webmedia: silent play prime present: {e}").into(),
                                );
                                p.arm_silent_play_clock(media_origin);
                            }
                        }
                    }
                }

                // Tab hidden: clock soft-held — do not present_due (would skip-catch).
                if let Ok(borrow) = player.try_borrow()
                    && borrow.as_ref().is_some_and(|p| p.is_hidden_hold())
                {
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
                        note_transport(&transport, TransportEvent::Ended);
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
                                let hidden = player
                                    .try_borrow()
                                    .ok()
                                    .and_then(|b| b.as_ref().map(|p| p.is_hidden_hold()))
                                    .unwrap_or(false);
                                let mut borrow = session.borrow_mut();
                                borrow.as_mut().is_some_and(|s| {
                                    if hidden
                                        || s.holding_for_buffer
                                        || s.resuming_clock
                                        || s.restarting
                                    {
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
        let transport = transport.clone();
        Rc::new(move || {
            let queue = queue.clone();
            let player = Rc::clone(&player);
            let session = Rc::clone(&session);
            let fps_meter = Rc::clone(&fps_meter);
            let sync_ui = Rc::clone(&sync_ui);
            let stop_playback = Rc::clone(&stop_playback);
            let resume_raf = resume_raf.clone();
            let transport = transport.clone();
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
                            if queue.is_current(id) && ready.get_untracked() {
                                note_transport(&transport, TransportEvent::OpenReady);
                            }
                        }
                        HostOp::Seek(target) => {
                            let _ = run_seek_or_step(
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
                            let _ = run_seek_or_step(
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
                            let _ = run_seek_or_step(
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
                        HostOp::ScrubRelease {
                            frame: target,
                            then_play,
                            settle_gen,
                        } => {
                            queue.set_scrub_inflight(true);
                            let ok = run_seek_or_step(
                                &queue,
                                id,
                                &player,
                                &sync_ui,
                                &session,
                                error,
                                playing,
                                playhead,
                                SeekOrStep::ScrubRelease { target, then_play },
                            )
                            .await;
                            queue.set_scrub_inflight(false);
                            let mut played_ok = false;
                            if ok && then_play && queue.is_current(id) {
                                played_ok = run_play_prime(
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
                            // Always note Done — reduce ignores stale `settle_gen`.
                            // Skipping when `!is_current` left transport in Settling
                            // after bump-without-Preempt (Play ignored / busy stuck).
                            let ok_final = if then_play {
                                played_ok && playing.get_untracked()
                            } else {
                                ok
                            };
                            note_transport(
                                &transport,
                                TransportEvent::ScrubReleaseDone {
                                    settle_gen,
                                    then_play,
                                    ok: ok_final,
                                },
                            );
                        }
                        HostOp::Step(forward) => {
                            let _ = run_seek_or_step(
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
                            let ok = run_play_prime(
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
                            if queue.is_current(id) {
                                note_transport(
                                    &transport,
                                    if ok {
                                        TransportEvent::PlayPrimeDone
                                    } else {
                                        TransportEvent::PlayPrimeFailed
                                    },
                                );
                            }
                        }
                    }
                }
            });
        }) as Rc<dyn Fn()>
    };

    // mediabunny media-player `initMediaPlayer`: pause → iterator.return →
    // asyncId++ before a new Input. Drop retiring VideoPlayer so UrlSource
    // dispose aborts Range workers (was: zombie orch after clip switch).
    let retire_clip = {
        let queue = queue.clone();
        let player = Rc::clone(&player);
        let session = Rc::clone(&session);
        let stop_playback = Rc::clone(&stop_playback);
        let fps_meter = Rc::clone(&fps_meter);
        let transport = transport.clone();
        Rc::new(move || {
            note_transport(&transport, TransportEvent::Closed);
            stop_playback();
            queue.cancel_all();
            playing.set(false);
            let _ = session.borrow_mut().take();
            // Drop *before* zeroing frame_count — paused rAF sync_ui treats
            // count 0→N as a new clip and would restore the old playhead.
            let retiring = player.borrow_mut().take();
            if let Some(p) = retiring {
                p.pause();
            }
            // `retiring` drops here → decoder/caches + UrlSource orch.dispose.
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
        }) as Rc<dyn Fn()>
    };

    let open_blob = {
        let queue = queue.clone();
        let retire_clip = Rc::clone(&retire_clip);
        let kick_drain = Rc::clone(&kick_drain);
        Rc::new(move |blob: web_sys::Blob| {
            let Some(canvas_el) = canvas_ref.get_untracked() else {
                error.set(Some("canvas not mounted".into()));
                status.set("Canvas missing — refresh the page.".into());
                return;
            };
            let canvas: HtmlCanvasElement = canvas_el;
            retire_clip();
            let size_kb = (blob.size() as u64) / 1024;
            status.set(OpenPhase::ProbingSource(OpenSourceKind::Progressive { size_kb }).label());
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
        let retire_clip = Rc::clone(&retire_clip);
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
            retire_clip();
            let fetch_id = queue.current_id();
            // Do **not** latch `busy` across resolve — cancelled fetch used to
            // leave busy=true forever when drain had already ended. `!ready`
            // keeps Play disabled; enqueue_open sets busy for demux/seek.
            // Same-origin /proxy bypasses CDN CORS (GitHub Releases, S3, …).
            let url = same_origin_media_proxy_url(&url).unwrap_or(url);
            status.set(OpenPhase::OpeningUrl { url: url.clone() }.label());
            #[cfg(target_arch = "wasm32")]
            web_sys::console::log_1(&format!("webmedia: open_url start {url}").into());

            let queue = queue.clone();
            let kick_drain = Rc::clone(&kick_drain);
            spawn_local(async move {
                let opened = resolve_url_source(&url, |phase| status.set(phase.label())).await;
                if !queue.is_current(fetch_id) {
                    queue.sync_busy();
                    return;
                }
                match opened {
                    Ok(source) => {
                        if queue.enqueue_open(source, canvas) {
                            kick_drain();
                        }
                    }
                    Err(e) => {
                        queue.sync_busy();
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
        let transport = transport.clone();
        let stop_playback = Rc::clone(&stop_playback);
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
            dispatch_transport(
                &transport,
                TransportEvent::PlayPressed,
                &queue,
                &kick_drain,
                &player,
                &session,
                playing,
                playhead,
                ended,
                &stop_playback,
            );
        }) as Rc<dyn Fn()>
    };

    let pause_fn = {
        let player = Rc::clone(&player);
        let session = Rc::clone(&session);
        let queue = queue.clone();
        let kick_drain = Rc::clone(&kick_drain);
        let transport = transport.clone();
        let stop_playback = Rc::clone(&stop_playback);
        Rc::new(move || {
            dispatch_transport(
                &transport,
                TransportEvent::PausePressed,
                &queue,
                &kick_drain,
                &player,
                &session,
                playing,
                playhead,
                ended,
                &stop_playback,
            );
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
    // Settle / release stays immediate via transport ScrubRelease.
    const SCRUB_DRAG_THROTTLE_MS: f64 = 50.0;

    let scrub_pointer_move_throttled = {
        let player = Rc::clone(&player);
        let session = Rc::clone(&session);
        let queue = queue.clone();
        let kick_drain = Rc::clone(&kick_drain);
        let transport = transport.clone();
        let stop_playback = Rc::clone(&stop_playback);
        use_throttle_fn_with_arg_and_options(
            move |target: u32| {
                dispatch_transport(
                    &transport,
                    TransportEvent::PointerMove { frame: target },
                    &queue,
                    &kick_drain,
                    &player,
                    &session,
                    playing,
                    playhead,
                    ended,
                    &stop_playback,
                );
            },
            SCRUB_DRAG_THROTTLE_MS,
            // trailing=false: a post-pointerup trailing drag can enqueue after
            // ScrubRelease and silently kill play.
            ThrottleOptions::default().leading(true).trailing(false),
        )
    };

    let scrub_pointer_down_fn = {
        let player = Rc::clone(&player);
        let session = Rc::clone(&session);
        let queue = queue.clone();
        let kick_drain = Rc::clone(&kick_drain);
        let transport = transport.clone();
        let stop_playback = Rc::clone(&stop_playback);
        Rc::new(move |target: u32| {
            dispatch_transport(
                &transport,
                TransportEvent::PointerDown { frame: target },
                &queue,
                &kick_drain,
                &player,
                &session,
                playing,
                playhead,
                ended,
                &stop_playback,
            );
        }) as Rc<dyn Fn(u32)>
    };

    let scrub_pointer_move_fn = {
        let scrub_pointer_move_throttled = scrub_pointer_move_throttled.clone();
        Rc::new(move |target: u32| {
            scrub_pointer_move_throttled(target);
        }) as Rc<dyn Fn(u32)>
    };

    let scrub_pointer_up_fn = {
        let player = Rc::clone(&player);
        let session = Rc::clone(&session);
        let queue = queue.clone();
        let kick_drain = Rc::clone(&kick_drain);
        let transport = transport.clone();
        let stop_playback = Rc::clone(&stop_playback);
        Rc::new(move |target: u32| {
            dispatch_transport(
                &transport,
                TransportEvent::PointerUp { frame: target },
                &queue,
                &kick_drain,
                &player,
                &session,
                playing,
                playhead,
                ended,
                &stop_playback,
            );
        }) as Rc<dyn Fn(u32)>
    };

    // Legacy settle/drag API removed — use scrub_pointer_* (transport FSM).

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

    let paint_hover_thumb_fn = {
        let player = Rc::clone(&player);
        Rc::new(
            move |frame: u32, dest: HtmlCanvasElement, max_w: u32, max_h: u32| {
                let Some(p) = player.borrow().as_ref().cloned() else {
                    return;
                };
                spawn_local(async move {
                    let _ = p
                        .paint_hover_thumb(u64::from(frame), &dest, max_w, max_h)
                        .await;
                });
            },
        ) as Rc<dyn Fn(u32, HtmlCanvasElement, u32, u32)>
    };

    let try_paint_hover_thumb_fn = {
        let player = Rc::clone(&player);
        Rc::new(
            move |frame: u32, dest: HtmlCanvasElement, max_w: u32, max_h: u32| {
                let Ok(borrow) = player.try_borrow() else {
                    return false;
                };
                let Some(p) = borrow.as_ref() else {
                    return false;
                };
                let aim = u64::from(frame);
                match p.try_paint_hover_thumb_cached(aim, &dest, max_w, max_h) {
                    // Far interim still paints, but return false so host defers
                    // throttled aim decode (Pause+Play far refine floor).
                    Ok(Some(painted)) => p.hover_thumb_close_enough(aim, painted),
                    _ => false,
                }
            },
        ) as Rc<dyn Fn(u32, HtmlCanvasElement, u32, u32) -> bool>
    };

    let cancel_hover_thumb_fn = {
        let player = Rc::clone(&player);
        Rc::new(move || {
            if let Ok(borrow) = player.try_borrow()
                && let Some(p) = borrow.as_ref()
            {
                p.cancel_hover_thumb();
            }
        }) as Rc<dyn Fn()>
    };

    let set_thumb_policy_fn = {
        let player = Rc::clone(&player);
        Rc::new(move |policy: webmedia::ThumbPolicy| {
            if let Ok(borrow) = player.try_borrow()
                && let Some(p) = borrow.as_ref()
            {
                p.set_thumb_policy(policy);
            }
        }) as Rc<dyn Fn(webmedia::ThumbPolicy)>
    };

    let close_fn = {
        let player = Rc::clone(&player);
        let session = Rc::clone(&session);
        let fps_meter = Rc::clone(&fps_meter);
        let queue = queue.clone();
        let transport = transport.clone();
        Rc::new(move || {
            note_transport(&transport, TransportEvent::Closed);
            queue.cancel_all();
            playing.set(false);
            let _ = session.borrow_mut().take();
            // Drop VideoPlayer (decoder + caches); cancel_ops via Drop.
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
        pause: pause_fn,
        close: Rc::clone(&close_fn),
        seek_frame: seek_fn,
        seek_keyframe: seek_keyframe_fn,
        scrub_pointer_down: scrub_pointer_down_fn,
        scrub_pointer_move: scrub_pointer_move_fn,
        scrub_pointer_up: scrub_pointer_up_fn,
        next_frame: next_fn,
        previous_frame: prev_fn,
        set_loop: set_loop_fn,
        set_volume: set_volume_fn,
        set_muted: set_muted_fn,
        set_playback_rate: set_playback_rate_fn,
        paint_hover_thumb: paint_hover_thumb_fn,
        try_paint_hover_thumb: try_paint_hover_thumb_fn,
        cancel_hover_thumb: cancel_hover_thumb_fn,
        set_thumb_policy: set_thumb_policy_fn,
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
