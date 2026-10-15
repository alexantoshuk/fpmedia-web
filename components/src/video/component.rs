//! [`Video`] component — canvas stage + playback orchestration.

use std::cell::Cell;
use std::rc::Rc;

use fpmedia_web::{BlobSource, MediaSource, OpenPhase, OpenSourceKind, PlayTick, TransportEvent};
use leptos::html::{Canvas, Div};
use leptos::prelude::*;
use leptos_use::utils::Pausable;
use leptos_use::{
    ThrottleOptions, UseRafFnOptions, use_event_listener, use_raf_fn_with_options,
    use_resize_observer, use_throttle_fn_with_arg_and_options,
};
use wasm_bindgen_futures::spawn_local;
use web_sys::HtmlCanvasElement;

use super::ctx::HostCtx;
use super::host_ops::{SeekOrStep, run_open, run_play_prime, run_seek_or_step};
use super::op_queue::{HostOp, OpQueue, OpenArgs};
use super::open_url::resolve_url_source;
use super::transport_apply::{dispatch_transport, new_transport, note_transport};
use super::{VideoOps, VideoRef};
use fpmedia_web::timer::{next_animation_frame, now_ms};

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
    // Paused / play rAF strip/`sync_ui` throttle (~10 Hz).
    let last_paused_ui_ms = Rc::new(Cell::new(0.0_f64));
    let last_play_ui_ms = Rc::new(Cell::new(0.0_f64));

    let VideoRef {
        ready,
        playing,
        looping,
        ended,
        fps,
        error,
        status,
        ..
    } = video;

    // mediabunny CallSerializer + asyncId — one queue for open / seek / play-prime.
    let ctx = HostCtx::new(video, OpQueue::new(video.busy), new_transport());

    // Tab / window hidden while playing: freeze clock + soft-hold audio so
    // restore does not skip-catch with silent AudioContext.
    // mediabunny media-player: no visibility handler — local adaptation.
    let visibility_held = Rc::new(Cell::new(false));
    let _visibility = {
        let ctx = ctx.clone();
        let visibility_held = Rc::clone(&visibility_held);
        let document = web_sys::window()
            .and_then(|w| w.document())
            .expect("document");
        use_event_listener(document.clone(), leptos::ev::visibilitychange, move |_| {
            let player = &ctx.player;
            if document.hidden() {
                if !playing.get_untracked() {
                    return;
                }
                let Ok(borrow) = player.try_borrow() else {
                    return;
                };
                let Some(p) = borrow.as_ref() else {
                    return;
                };
                if !p.play_session_live() {
                    return;
                }
                if p.hold_media_clock_for_hidden().is_some() {
                    visibility_held.set(true);
                    web_sys::console::log_1(&"fpmedia-web: visibility hold".into());
                }
                return;
            }
            if !visibility_held.get() {
                return;
            }
            visibility_held.set(false);
            if !playing.get_untracked() {
                return;
            }
            let player = Rc::clone(player);
            spawn_local(async move {
                let Some(p) = player.borrow().as_ref().cloned() else {
                    return;
                };
                if !p.play_session_live() {
                    return;
                }
                if let Err(e) = p.resume_audio_context().await {
                    web_sys::console::warn_1(
                        &format!("fpmedia-web: visibility audio resume: {e}").into(),
                    );
                }
                match p.resume_after_hold().await {
                    Ok(()) => {
                        web_sys::console::log_1(&"fpmedia-web: visibility resume".into());
                    }
                    Err(e) => {
                        web_sys::console::warn_1(
                            &format!("fpmedia-web: visibility clock resume: {e}").into(),
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
            let ctx = ctx.clone();
            let last_paused_ui_ms = Rc::clone(&last_paused_ui_ms);
            let last_play_ui_ms = Rc::clone(&last_play_ui_ms);
            move |_| {
                let HostCtx {
                    player,
                    fps_meter,
                    transport,
                    ..
                } = &ctx;
                // Play path: engine present tick + UI throttle.
                // Pause path: still tick so gap-fill strip marks update (fill is async).
                if !playing.get_untracked() {
                    // Match play ~10 Hz — unthrottled paused rAF + strip Effect
                    // starved pointer/playhead CSS (red bar lagged the finger).
                    // Drag: ~150 ms (hover coalesce) so strips still grow.
                    let interval_ms = if transport.is_scrubbing() {
                        150.0
                    } else {
                        100.0
                    };
                    let now = now_ms().unwrap_or(0.0);
                    if now - last_paused_ui_ms.get() < interval_ms {
                        return;
                    }
                    last_paused_ui_ms.set(now);
                    // `sync_ui` refreshes pipeline + frame_count (playhead only when
                    // count changes / playing — preserves scrub aim).
                    ctx.sync_ui();
                    return;
                }

                {
                    let want = looping.get_untracked();
                    ctx.with_player(|p| p.set_loop(want));
                }

                let Some(p) = player.borrow().as_ref().cloned() else {
                    ctx.stop_playback();
                    return;
                };
                // Armed only after prime — skip while priming so Ended cannot
                // bump async_id mid-PlayPrime.
                if !p.play_session_live() {
                    return;
                }

                match p.play_raf_tick() {
                    Ok(PlayTick::Skipped) => {}
                    Ok(PlayTick::Ended) => {
                        ended.set(true);
                        note_transport(&ctx, TransportEvent::Ended);
                        ctx.stop_playback();
                        status.set("Ended.".into());
                    }
                    Ok(PlayTick::Presented { did_draw }) => {
                        let now = now_ms();
                        if did_draw
                            && let Some(measured) = fps_meter.borrow_mut().note_present()
                            && now.is_some_and(|n| n - last_play_ui_ms.get() >= 100.0)
                        {
                            fps.set(Some(measured));
                        }
                        if let Some(n) = now
                            && n - last_play_ui_ms.get() >= 100.0
                        {
                            last_play_ui_ms.set(n);
                            ctx.sync_ui();
                        }
                    }
                    Ok(PlayTick::Failed(msg)) => {
                        error.set(Some(msg));
                        ctx.stop_playback();
                    }
                    Err(e) => {
                        error.set(Some(e.to_string()));
                        ctx.stop_playback();
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
        let ctx = ctx.clone();
        move |_entries, _observer| {
            ctx.with_player(|p| {
                let _ = p.relayout();
            });
        }
    });

    let kick_drain = {
        let ctx = ctx.clone();
        let resume_raf = resume_raf.clone();
        Rc::new(move || {
            let ctx = ctx.clone();
            let resume_raf = resume_raf.clone();
            // IMPORTANT: wasm_bindgen_futures::spawn_local (not leptos) — open may
            // be invoked from the file-input handler outside a reactive owner.
            spawn_local(async move {
                let queue = &ctx.queue;
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
                        HostOp::Open(args) => {
                            run_open(&ctx, id, args).await;
                            if queue.is_current(id) && ready.get_untracked() {
                                note_transport(&ctx, TransportEvent::OpenReady);
                            }
                        }
                        HostOp::Seek(target) => {
                            let _ = run_seek_or_step(&ctx, id, SeekOrStep::Seek(target)).await;
                        }
                        HostOp::SeekKeyframe(target) => {
                            let _ =
                                run_seek_or_step(&ctx, id, SeekOrStep::SeekKeyframe(target)).await;
                        }
                        HostOp::ScrubDrag(target) => {
                            queue.set_scrub_inflight(true);
                            let _ = run_seek_or_step(&ctx, id, SeekOrStep::ScrubDrag(target)).await;
                            queue.set_scrub_inflight(false);
                        }
                        HostOp::ScrubRelease {
                            frame: target,
                            then_play,
                            settle_gen,
                        } => {
                            queue.set_scrub_inflight(true);
                            let ok = run_seek_or_step(
                                &ctx,
                                id,
                                SeekOrStep::ScrubRelease { target, then_play },
                            )
                            .await;
                            queue.set_scrub_inflight(false);
                            let mut played_ok = false;
                            if ok && then_play && queue.is_current(id) {
                                played_ok = run_play_prime(&ctx, id, &resume_raf).await;
                            }
                            // Always note Done — reduce ignores stale `settle_gen`.
                            // Skipping when `!is_current` left transport in Settling
                            // after bump-without-Preempt (Play ignored / busy stuck).
                            let ok_final = if then_play { played_ok } else { ok };
                            note_transport(
                                &ctx,
                                TransportEvent::ScrubReleaseDone {
                                    settle_gen,
                                    then_play,
                                    ok: ok_final,
                                },
                            );
                        }
                        HostOp::Step(forward) => {
                            let _ = run_seek_or_step(&ctx, id, SeekOrStep::Step(forward)).await;
                            // Cached steps can finish in one microtask; without a
                            // paint yield the drain chains several frames before
                            // the browser composites → lag then a burst.
                            next_animation_frame().await;
                        }
                        HostOp::PlayPrime => {
                            let ok = run_play_prime(&ctx, id, &resume_raf).await;
                            if queue.is_current(id) {
                                note_transport(
                                    &ctx,
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

    let open_blob = {
        let ctx = ctx.clone();
        let kick_drain = Rc::clone(&kick_drain);
        Rc::new(move |blob: web_sys::Blob| {
            let Some(canvas_el) = canvas_ref.get_untracked() else {
                error.set(Some("canvas not mounted".into()));
                status.set("Canvas missing — refresh the page.".into());
                return;
            };
            let canvas: HtmlCanvasElement = canvas_el;
            ctx.retire_clip();
            let size_kb = (blob.size() as u64) / 1024;
            status.set(OpenPhase::ProbingSource(OpenSourceKind::Progressive { size_kb }).label());
            #[cfg(target_arch = "wasm32")]
            web_sys::console::log_1(&format!("fpmedia-web: open_blob start ~{size_kb} KB").into());
            let source: std::sync::Arc<dyn MediaSource> =
                std::sync::Arc::new(BlobSource::new(blob));
            if ctx.queue.enqueue(HostOp::Open(OpenArgs { source, canvas })) {
                kick_drain();
            }
        }) as Rc<dyn Fn(web_sys::Blob)>
    };

    let open_url = {
        let ctx = ctx.clone();
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
            ctx.retire_clip();
            let fetch_id = ctx.queue.current_id();
            // Do **not** latch `busy` across resolve — cancelled fetch used to
            // leave busy=true forever when drain had already ended. `!ready`
            // keeps Play disabled; Open enqueue sets busy for demux/seek.
            // Cross-origin: host must pass a CORS-ok or same-origin URL.
            status.set(OpenPhase::OpeningUrl { url: url.clone() }.label());
            #[cfg(target_arch = "wasm32")]
            web_sys::console::log_1(&format!("fpmedia-web: open_url start {url}").into());

            let queue = ctx.queue.clone();
            let kick_drain = Rc::clone(&kick_drain);
            spawn_local(async move {
                let opened = resolve_url_source(&url, |phase| status.set(phase.label())).await;
                if !queue.is_current(fetch_id) {
                    queue.sync_busy();
                    return;
                }
                match opened {
                    Ok(source) => {
                        if queue.enqueue(HostOp::Open(OpenArgs { source, canvas })) {
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

    let dispatch = {
        let ctx = ctx.clone();
        let kick_drain = Rc::clone(&kick_drain);
        Rc::new(move |event: TransportEvent| dispatch_transport(&ctx, &kick_drain, event))
            as Rc<dyn Fn(TransportEvent)>
    };
    let play_fn = {
        let ctx = ctx.clone();
        let dispatch = Rc::clone(&dispatch);
        Rc::new(move || {
            if !ready.get_untracked() {
                web_sys::console::log_1(&"fpmedia-web: play ignored (not ready)".into());
                return;
            }
            // Transport already in a live play session (Pause race / double Play).
            let live = ctx
                .player
                .borrow()
                .as_ref()
                .is_some_and(|p| p.play_session_live());
            if ctx.transport.state().ui_playing(live) {
                if live {
                    web_sys::console::log_1(&"fpmedia-web: play ignored (already playing)".into());
                    return;
                }
                web_sys::console::log_1(&"fpmedia-web: recover stale transport playing".into());
                ctx.stop_playback();
            }
            if ctx.player.borrow().as_ref().is_none() {
                web_sys::console::log_1(&"fpmedia-web: play ignored (no player)".into());
                return;
            }
            dispatch(TransportEvent::PlayPressed);
        }) as Rc<dyn Fn()>
    };

    let pause_fn = {
        let dispatch = Rc::clone(&dispatch);
        Rc::new(move || dispatch(TransportEvent::PausePressed)) as Rc<dyn Fn()>
    };

    let scrub_pointer_down_fn = {
        let dispatch = Rc::clone(&dispatch);
        Rc::new(move |frame: u32| dispatch(TransportEvent::PointerDown { frame }))
            as Rc<dyn Fn(u32)>
    };

    let scrub_pointer_up_fn = {
        let dispatch = Rc::clone(&dispatch);
        Rc::new(move |frame: u32| dispatch(TransportEvent::PointerUp { frame })) as Rc<dyn Fn(u32)>
    };

    // Rate-limit scrub drag only — pointermove is denser than key-repeat.
    // Settle / release stays immediate via transport ScrubRelease.
    const SCRUB_DRAG_THROTTLE_MS: f64 = 50.0;

    let scrub_pointer_move_throttled = {
        let dispatch = Rc::clone(&dispatch);
        use_throttle_fn_with_arg_and_options(
            move |frame: u32| dispatch(TransportEvent::PointerMove { frame }),
            SCRUB_DRAG_THROTTLE_MS,
            // trailing=false: a post-pointerup trailing drag can enqueue after
            // ScrubRelease and silently kill play.
            ThrottleOptions::default().leading(true).trailing(false),
        )
    };

    let scrub_pointer_move_fn = Rc::new(move |target: u32| {
        scrub_pointer_move_throttled(target);
    }) as Rc<dyn Fn(u32)>;

    let seek_frame_fn = {
        let dispatch = Rc::clone(&dispatch);
        Rc::new(move |frame: u32| dispatch(TransportEvent::SeekRequested { frame }))
            as Rc<dyn Fn(u32)>
    };
    let seek_keyframe_fn = {
        let dispatch = Rc::clone(&dispatch);
        Rc::new(move |frame: u32| dispatch(TransportEvent::SeekKeyframeRequested { frame }))
            as Rc<dyn Fn(u32)>
    };
    let step_fn = |forward: bool| {
        let dispatch = Rc::clone(&dispatch);
        Rc::new(move || dispatch(TransportEvent::StepRequested { forward })) as Rc<dyn Fn()>
    };

    let set_loop_fn = {
        let ctx = ctx.clone();
        Rc::new(move |on: bool| {
            ctx.with_player(|p| p.set_loop(on));
            ctx.sync_ui();
        }) as Rc<dyn Fn(bool)>
    };

    let set_volume_fn = {
        let ctx = ctx.clone();
        Rc::new(move |v: f64| {
            ctx.with_player(|p| p.set_volume(v));
        }) as Rc<dyn Fn(f64)>
    };

    let set_muted_fn = {
        let ctx = ctx.clone();
        Rc::new(move |m: bool| {
            ctx.with_player(|p| p.set_muted(m));
        }) as Rc<dyn Fn(bool)>
    };

    let set_playback_rate_fn = {
        let ctx = ctx.clone();
        Rc::new(move |r: f64| {
            ctx.with_player(|p| p.set_playback_rate(r));
        }) as Rc<dyn Fn(f64)>
    };

    let paint_hover_thumb_fn = {
        let ctx = ctx.clone();
        Rc::new(
            move |frame: u32, dest: HtmlCanvasElement, max_w: u32, max_h: u32| {
                let Some(p) = ctx.player.borrow().as_ref().cloned() else {
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
        let ctx = ctx.clone();
        Rc::new(
            move |frame: u32, dest: HtmlCanvasElement, max_w: u32, max_h: u32| {
                ctx.with_player(|p| {
                    let aim = u64::from(frame);
                    match p.try_paint_hover_thumb_cached(aim, &dest, max_w, max_h) {
                        // Far interim still paints, but return false so host defers
                        // throttled aim decode (Pause+Play far refine floor).
                        Ok(Some(painted)) => p.hover_thumb_close_enough(aim, painted),
                        _ => false,
                    }
                })
                .unwrap_or(false)
            },
        ) as Rc<dyn Fn(u32, HtmlCanvasElement, u32, u32) -> bool>
    };

    let cancel_hover_thumb_fn = {
        let ctx = ctx.clone();
        Rc::new(move || {
            ctx.with_player(|p| p.cancel_hover_thumb());
        }) as Rc<dyn Fn()>
    };

    let set_thumb_policy_fn = {
        let ctx = ctx.clone();
        Rc::new(move |policy: fpmedia_web::ThumbPolicy| {
            ctx.with_player(|p| p.set_thumb_policy(policy));
        }) as Rc<dyn Fn(fpmedia_web::ThumbPolicy)>
    };

    let close_fn = {
        let ctx = ctx.clone();
        Rc::new(move || {
            ctx.retire_clip();
            status.set("Closed.".into());
        }) as Rc<dyn Fn()>
    };

    video.ops.set_value(Some(VideoOps {
        open_blob,
        open_url,
        play: play_fn,
        pause: pause_fn,
        close: Rc::clone(&close_fn),
        seek_frame: seek_frame_fn,
        seek_keyframe: seek_keyframe_fn,
        scrub_pointer_down: scrub_pointer_down_fn,
        scrub_pointer_move: scrub_pointer_move_fn,
        scrub_pointer_up: scrub_pointer_up_fn,
        next_frame: step_fn(true),
        previous_frame: step_fn(false),
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
