//! [`Video`] component — canvas stage + playback orchestration.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use leptos::html::{Canvas, Div};
use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos_use::utils::Pausable;
use leptos_use::{use_raf_fn_with_options, use_resize_observer, UseRafFnOptions};
use wasm_bindgen::JsCast;
use web_sys::{AudioContext, HtmlCanvasElement};
use webmedia::{DuePresent, WebPlayer};

use crate::{VideoOps, VideoRef};

type PlayerHandle = Rc<RefCell<Option<WebPlayer>>>;

/// Media-clock + UI throttle state for an active play session.
struct PlaySession {
    audio: Option<AudioContext>,
    media_origin: f64,
    audio_origin: f64,
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
    // Bumped on each open / stop so late async results are discarded.
    let open_epoch = Rc::new(Cell::new(0_u64));
    let play_epoch = Rc::new(Cell::new(0_u64));

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
                playing.set(st.playing);
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
        let play_epoch = Rc::clone(&play_epoch);
        Rc::new(move || {
            play_epoch.set(play_epoch.get().wrapping_add(1));
            playing.set(false);
            if let Some(prev) = session.borrow_mut().take() {
                if let Some(audio) = prev.audio {
                    let _ = audio.close();
                }
            }
            if let Ok(borrow) = player.try_borrow() {
                if let Some(p) = borrow.as_ref() {
                    p.pause();
                }
            }
            fps_meter.borrow_mut().reset();
            fps.set(None);
            sync_ui();
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
                    if let Some(a) = s.audio.as_ref() {
                        a.current_time() - s.audio_origin + s.media_origin
                    } else if let Some(now) = performance_now_ms() {
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
                            let restarted = {
                                let borrow = player.borrow();
                                match borrow.as_ref() {
                                    Some(p) => Some(p.loop_to_start().await),
                                    None => None,
                                }
                            };
                            match restarted {
                                Some(Ok(())) => {
                                    if let Some(s) = session.borrow_mut().as_mut() {
                                        s.media_origin = 0.0;
                                        s.audio_origin = s
                                            .audio
                                            .as_ref()
                                            .map(|a| a.current_time())
                                            .unwrap_or(0.0);
                                        s.wall_origin = performance_now_ms()
                                            .map(|n| n / 1000.0)
                                            .unwrap_or(0.0);
                                        s.restarting = false;
                                    }
                                }
                                Some(Err(e)) if e.is_cancelled() => {
                                    if let Some(s) = session.borrow_mut().as_mut() {
                                        s.restarting = false;
                                    }
                                }
                                Some(Err(e)) => {
                                    error.set(Some(e.to_string()));
                                    stop_playback();
                                }
                                None => stop_playback(),
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

    let open_bytes = {
        let player = Rc::clone(&player);
        let sync_ui = Rc::clone(&sync_ui);
        let fps_meter = Rc::clone(&fps_meter);
        let stop_playback = Rc::clone(&stop_playback);
        let open_epoch = Rc::clone(&open_epoch);
        Rc::new(move |bytes: Vec<u8>| {
            let Some(canvas_el) = canvas_ref.get_untracked() else {
                error.set(Some("canvas not mounted".into()));
                status.set("Canvas missing — refresh the page.".into());
                return;
            };
            let canvas: HtmlCanvasElement = canvas_el;
            if busy.get_untracked() {
                return;
            }
            stop_playback();
            let my_open = open_epoch.get().wrapping_add(1);
            open_epoch.set(my_open);
            busy.set(true);
            ready.set(false);
            error.set(None);
            fps.set(None);
            pipeline.set(None);
            metadata.set(None);
            status.set(format!("Opening ({} KB)…", bytes.len() / 1024));
            fps_meter.borrow_mut().reset();
            let player = Rc::clone(&player);
            let sync_ui = Rc::clone(&sync_ui);
            let open_epoch = Rc::clone(&open_epoch);
            spawn_local(async move {
                *player.borrow_mut() = None;
                status.set(format!("Demux + decoder ({} KB)…", bytes.len() / 1024));
                let opened = open_with_timeout(bytes, canvas, 15_000).await;
                if open_epoch.get() != my_open {
                    busy.set(false);
                    return;
                }
                match opened {
                    Ok(p) => {
                        metadata.set(Some(p.metadata()));
                        let _ = p.relayout();
                        *player.borrow_mut() = Some(p);
                        status.set("Decoding frame 0…".into());
                        let seek = {
                            let Some(p) = player.borrow_mut().take() else {
                                error.set(Some("player lost after open".into()));
                                status.set("Open failed: player lost".into());
                                busy.set(false);
                                return;
                            };
                            let result = p.seek_frame(0).await;
                            *player.borrow_mut() = Some(p);
                            result
                        };
                        if open_epoch.get() != my_open {
                            busy.set(false);
                            return;
                        }
                        match seek {
                            Ok(()) => {
                                ready.set(true);
                                status.set("Ready.".into());
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
                    }
                }
                busy.set(false);
            });
        }) as Rc<dyn Fn(Vec<u8>)>
    };

    let play_fn = {
        let player = Rc::clone(&player);
        let session = Rc::clone(&session);
        let fps_meter = Rc::clone(&fps_meter);
        let resume_raf = resume_raf.clone();
        let stop_playback = Rc::clone(&stop_playback);
        let play_epoch = Rc::clone(&play_epoch);
        Rc::new(move || {
            if busy.get_untracked() || !ready.get_untracked() || playing.get_untracked() {
                return;
            }
            if let Some(p) = player.borrow().as_ref() {
                p.play();
            }
            fps_meter.borrow_mut().reset();
            fps.set(None);
            let my_play = play_epoch.get().wrapping_add(1);
            play_epoch.set(my_play);
            playing.set(true);
            busy.set(true);
            let player = Rc::clone(&player);
            let session = Rc::clone(&session);
            let resume_raf = resume_raf.clone();
            let stop_playback = Rc::clone(&stop_playback);
            let play_epoch = Rc::clone(&play_epoch);
            spawn_local(async move {
                let audio = match AudioContext::new() {
                    Ok(ctx) => {
                        if let Ok(p) = ctx.resume() {
                            let _ = wasm_bindgen_futures::JsFuture::from(p).await;
                        }
                        Some(ctx)
                    }
                    Err(_) => None,
                };

                let prime_ok = {
                    let borrow = player.borrow();
                    if let Some(p) = borrow.as_ref() {
                        match p.prime_playback().await {
                            Ok(()) => true,
                            Err(e) if e.is_cancelled() => false,
                            Err(e) => {
                                error.set(Some(e.to_string()));
                                false
                            }
                        }
                    } else {
                        false
                    }
                };

                if !prime_ok || play_epoch.get() != my_play || !playing.get_untracked() {
                    if let Some(a) = audio {
                        let _ = a.close();
                    }
                    busy.set(false);
                    if play_epoch.get() == my_play {
                        stop_playback();
                    }
                    return;
                }

                let media_origin = {
                    let borrow = player.borrow();
                    match borrow.as_ref() {
                        Some(p) => p.state().timestamp.seconds,
                        None => {
                            if let Some(a) = audio {
                                let _ = a.close();
                            }
                            busy.set(false);
                            stop_playback();
                            return;
                        }
                    }
                };
                let audio_origin = audio.as_ref().map(|a| a.current_time()).unwrap_or(0.0);
                let wall_origin = performance_now_ms().map(|t| t / 1000.0).unwrap_or(0.0);
                let last_ui_ms = performance_now_ms().unwrap_or(0.0);

                *session.borrow_mut() = Some(PlaySession {
                    audio,
                    media_origin,
                    audio_origin,
                    wall_origin,
                    last_ui_ms,
                    restarting: false,
                });
                busy.set(false);

                {
                    let player = Rc::clone(&player);
                    let stop_playback = Rc::clone(&stop_playback);
                    let play_epoch = Rc::clone(&play_epoch);
                    spawn_local(async move {
                        while playing.get_untracked() && play_epoch.get() == my_play {
                            let need = {
                                let borrow = player.borrow();
                                borrow.as_ref().is_some_and(|p| {
                                    p.is_playing()
                                        && !p.is_restarting()
                                        && p.presentation_len()
                                            < webmedia::PRESENTATION_QUEUE_CAP
                                })
                            };
                            if need {
                                let decoded = {
                                    let borrow = player.borrow();
                                    match borrow.as_ref() {
                                        Some(p) => p.prefetch_one().await,
                                        None => break,
                                    }
                                };
                                match decoded {
                                    Ok(true) => continue,
                                    Ok(false) => {}
                                    Err(e) if e.is_cancelled() => break,
                                    Err(e) => {
                                        error.set(Some(e.to_string()));
                                        stop_playback();
                                        break;
                                    }
                                }
                            }
                            {
                                let prepare = {
                                    let borrow = player.borrow();
                                    match borrow.as_ref() {
                                        Some(p) if p.is_looping() => {
                                            Some(p.prepare_loop_if_needed().await)
                                        }
                                        _ => None,
                                    }
                                };
                                match prepare {
                                    Some(Err(e)) if e.is_cancelled() => break,
                                    Some(Err(e)) => {
                                        error.set(Some(e.to_string()));
                                        stop_playback();
                                        break;
                                    }
                                    _ => {}
                                }
                            }
                            wait_animation_frame().await;
                        }
                    });
                }

                resume_raf();
            });
        }) as Rc<dyn Fn()>
    };

    let seek_fn = {
        let player = Rc::clone(&player);
        let sync_ui = Rc::clone(&sync_ui);
        Rc::new(move |target: u32| {
            step_or_seek(
                &player,
                &sync_ui,
                busy,
                error,
                playing,
                SeekOrStep::Seek(target),
            );
        }) as Rc<dyn Fn(u32)>
    };

    let next_fn = {
        let player = Rc::clone(&player);
        let sync_ui = Rc::clone(&sync_ui);
        Rc::new(move || {
            step_or_seek(
                &player,
                &sync_ui,
                busy,
                error,
                playing,
                SeekOrStep::Step(true),
            );
        }) as Rc<dyn Fn()>
    };

    let prev_fn = {
        let player = Rc::clone(&player);
        let sync_ui = Rc::clone(&sync_ui);
        Rc::new(move || {
            step_or_seek(
                &player,
                &sync_ui,
                busy,
                error,
                playing,
                SeekOrStep::Step(false),
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
        open: open_bytes,
        play: play_fn,
        pause: stop_playback,
        seek_frame: seek_fn,
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

enum SeekOrStep {
    Seek(u32),
    Step(bool),
}

fn step_or_seek(
    player: &PlayerHandle,
    sync_ui: &Rc<dyn Fn()>,
    busy: RwSignal<bool>,
    error: RwSignal<Option<String>>,
    playing: RwSignal<bool>,
    op: SeekOrStep,
) {
    if busy.get_untracked() {
        return;
    }
    busy.set(true);
    playing.set(false);
    let player = Rc::clone(player);
    let sync_ui = Rc::clone(sync_ui);
    spawn_local(async move {
        let Some(p) = player.borrow_mut().take() else {
            busy.set(false);
            return;
        };
        p.pause();
        let result = match op {
            SeekOrStep::Seek(target) => p.seek_frame(u64::from(target)).await,
            SeekOrStep::Step(true) => p.next_frame().await,
            SeekOrStep::Step(false) => p.previous_frame().await,
        };
        if let Err(e) = &result {
            if !e.is_cancelled() {
                error.set(Some(e.to_string()));
            }
        }
        *player.borrow_mut() = Some(p);
        sync_ui();
        busy.set(false);
    });
}

async fn open_with_timeout(
    bytes: Vec<u8>,
    canvas: HtmlCanvasElement,
    timeout_ms: i32,
) -> Result<WebPlayer, String> {
    let outcome: Rc<RefCell<Option<Result<WebPlayer, String>>>> = Rc::new(RefCell::new(None));
    {
        let outcome = Rc::clone(&outcome);
        spawn_local(async move {
            let result = WebPlayer::open_mp4_bytes(bytes, canvas)
                .await
                .map_err(|e| e.to_string());
            *outcome.borrow_mut() = Some(result);
        });
    }

    let start = performance_now_ms().unwrap_or(0.0);
    loop {
        if let Some(result) = outcome.borrow_mut().take() {
            return result;
        }
        let now = performance_now_ms().unwrap_or(start);
        if now - start >= f64::from(timeout_ms) {
            return Err(format!(
                "timed out after {timeout_ms}ms (open/decode hung — hard-refresh and check console)"
            ));
        }
        sleep_ms(50).await;
    }
}

async fn sleep_ms(ms: i32) {
    let promise = js_sys::Promise::new(&mut |resolve, _reject| {
        let _ = web_sys::window()
            .expect("window")
            .set_timeout_with_callback_and_timeout_and_arguments_0(&resolve, ms);
    });
    let _ = wasm_bindgen_futures::JsFuture::from(promise).await;
}

/// Await one animation frame without `Closure::forget` (pass Promise resolve to rAF).
async fn wait_animation_frame() {
    let promise = js_sys::Promise::new(&mut |resolve, _reject| {
        let window = web_sys::window().expect("window");
        let _ = window.request_animation_frame(resolve.unchecked_ref());
    });
    let _ = wasm_bindgen_futures::JsFuture::from(promise).await;
}

fn performance_now_ms() -> Option<f64> {
    web_sys::window()?.performance().map(|p| p.now())
}
