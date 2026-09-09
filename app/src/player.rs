//! Leptos demo controls over [`webmedia::WebPlayer`].
//!
//! Frame-oriented transport (seek/step by `FrameId`) with a `<video>`-like
//! media stage: fixed viewport, contain layout, and source metadata.

use std::cell::RefCell;
use std::rc::Rc;

use js_sys::Uint8Array;
use leptos::html::{Canvas, Div};
use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos_use::utils::Pausable;
use leptos_use::{
    use_raf_fn_with_options, use_resize_observer, UseRafFnOptions,
};
use wasm_bindgen::JsCast;
use web_sys::{AudioContext, HtmlCanvasElement, HtmlInputElement};
use webmedia::{DuePresent, PipelineStats, VideoMetadata, WebPlayer};

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
    /// `performance.now()` timestamps (ms) of recent presents.
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

    /// Record a present and return FPS over the last ~1s (needs ≥2 samples).
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

#[component]
pub fn PlayerDemo() -> impl IntoView {
    let canvas_ref = NodeRef::<Canvas>::new();
    let viewport_ref = NodeRef::<Div>::new();
    let player: PlayerHandle = Rc::new(RefCell::new(None));
    let fps_meter = Rc::new(RefCell::new(FpsMeter::new()));
    let session: Rc<RefCell<Option<PlaySession>>> = Rc::new(RefCell::new(None));

    let busy = RwSignal::new(false);
    let error = RwSignal::new(Option::<String>::None);
    let frame = RwSignal::new(0_u32);
    let frame_count = RwSignal::new(0_u32);
    let timestamp = RwSignal::new(0.0_f64);
    // Measured display FPS while playing (`None` when idle / not enough samples).
    let fps = RwSignal::new(Option::<f64>::None);
    let pipeline = RwSignal::new(Option::<PipelineStats>::None);
    let meta = RwSignal::new(Option::<VideoMetadata>::None);
    let playing = RwSignal::new(false);
    let looping = RwSignal::new(false);
    let ready = RwSignal::new(false);
    let status = RwSignal::new(String::from("Load an MP4 to begin."));

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
                meta.set(Some(p.metadata()));
            }
        }) as Rc<dyn Fn()>
    };

    let stop_playback = {
        let player = Rc::clone(&player);
        let session = Rc::clone(&session);
        let fps_meter = Rc::clone(&fps_meter);
        let sync_ui = Rc::clone(&sync_ui);
        Rc::new(move || {
            playing.set(false);
            *session.borrow_mut() = None;
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

    // Present loop — paused until Play finishes priming the media clock.
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

    // Stop rAF whenever UI play state clears (Pause, EOF, errors).
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
            busy.set(true);
            ready.set(false);
            error.set(None);
            fps.set(None);
            pipeline.set(None);
            meta.set(None);
            status.set(format!("Opening ({} KB)…", bytes.len() / 1024));
            fps_meter.borrow_mut().reset();
            let player = Rc::clone(&player);
            let sync_ui = Rc::clone(&sync_ui);
            spawn_local(async move {
                *player.borrow_mut() = None;
                status.set(format!("Demux + decoder ({} KB)…", bytes.len() / 1024));
                let opened = open_with_timeout(bytes, canvas, 15_000).await;
                match opened {
                    Ok(p) => {
                        meta.set(Some(p.metadata()));
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
                        match seek {
                            Ok(()) => {
                                ready.set(true);
                                status.set("Ready.".into());
                                sync_ui();
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
                        meta.set(None);
                        error.set(Some(e.clone()));
                        status.set(format!("Open failed: {e}"));
                    }
                }
                busy.set(false);
            });
        }) as Rc<dyn Fn(Vec<u8>)>
    };

    view! {
        <div class="wm-player">
            <div class="wm-viewport" node_ref=viewport_ref>
                <canvas
                    node_ref=canvas_ref
                    class="wm-canvas"
                    width="320"
                    height="240"
                />
            </div>
            <p class="wm-meta">
                {move || meta.get().as_ref().map(format_metadata).unwrap_or_default()}
            </p>
            <div class="wm-controls">
                <label class="wm-file">
                    "Open MP4"
                    <input
                        type="file"
                        accept="video/mp4,video/*"
                        on:change={
                            let open_bytes = Rc::clone(&open_bytes);
                            move |ev: leptos::ev::Event| {
                                let input: HtmlInputElement = event_target(&ev);
                                let Some(files) = input.files() else { return };
                                let Some(file) = files.get(0) else { return };
                                let open_bytes = Rc::clone(&open_bytes);
                                spawn_local(async move {
                                    match wasm_bindgen_futures::JsFuture::from(file.array_buffer()).await {
                                        Ok(buf) => open_bytes(Uint8Array::new(&buf).to_vec()),
                                        Err(e) => error.set(Some(format!("{e:?}"))),
                                    }
                                });
                            }
                        }
                    />
                </label>
                <button
                    type="button"
                    class="wm-btn wm-btn-primary"
                    prop:disabled=move || busy.get() || !ready.get() || playing.get()
                    on:click={
                        let player = Rc::clone(&player);
                        let session = Rc::clone(&session);
                        let fps_meter = Rc::clone(&fps_meter);
                        let resume_raf = resume_raf.clone();
                        let stop_playback = Rc::clone(&stop_playback);
                        move |_| {
                            if busy.get_untracked() || !ready.get_untracked() || playing.get_untracked() {
                                return;
                            }
                            if let Some(p) = player.borrow().as_ref() {
                                p.play();
                            }
                            fps_meter.borrow_mut().reset();
                            fps.set(None);
                            playing.set(true);
                            let player = Rc::clone(&player);
                            let session = Rc::clone(&session);
                            let resume_raf = resume_raf.clone();
                            let stop_playback = Rc::clone(&stop_playback);
                            spawn_local(async move {
                                // Mediabunny-style: AudioContext clock + rAF present,
                                // with decode on a *separate* task so presents never
                                // wait on WebCodecs.
                                let audio = match AudioContext::new() {
                                    Ok(ctx) => {
                                        if let Ok(p) = ctx.resume() {
                                            let _ = wasm_bindgen_futures::JsFuture::from(p).await;
                                        }
                                        Some(ctx)
                                    }
                                    Err(_) => None,
                                };

                                {
                                    let borrow = player.borrow();
                                    if let Some(p) = borrow.as_ref() {
                                        if let Err(e) = p.prime_playback().await {
                                            error.set(Some(e.to_string()));
                                            stop_playback();
                                            return;
                                        }
                                    } else {
                                        stop_playback();
                                        return;
                                    }
                                }

                                // Clock origins AFTER prime — priming can take hundreds of ms;
                                // starting the media clock beforehand makes present_due skip
                                // immediately and feels like start stutter (~15 fps).
                                let media_origin = {
                                    let borrow = player.borrow();
                                    match borrow.as_ref() {
                                        Some(p) => p.state().timestamp.seconds,
                                        None => {
                                            stop_playback();
                                            return;
                                        }
                                    }
                                };
                                let audio_origin =
                                    audio.as_ref().map(|a| a.current_time()).unwrap_or(0.0);
                                let wall_origin =
                                    performance_now_ms().map(|t| t / 1000.0).unwrap_or(0.0);
                                let last_ui_ms = performance_now_ms().unwrap_or(0.0);

                                *session.borrow_mut() = Some(PlaySession {
                                    audio,
                                    media_origin,
                                    audio_origin,
                                    wall_origin,
                                    last_ui_ms,
                                    restarting: false,
                                });

                                // Background decode pump (await-rAF yield — not use_raf_fn).
                                {
                                    let player = Rc::clone(&player);
                                    let stop_playback = Rc::clone(&stop_playback);
                                    spawn_local(async move {
                                        while playing.get_untracked() {
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
                                                    Ok(true) => continue, // keep filling to CAP
                                                    Ok(false) => {
                                                        // EOF / full / paused — must yield or we spin.
                                                    }
                                                    Err(e) => {
                                                        error.set(Some(e.to_string()));
                                                        stop_playback();
                                                        break;
                                                    }
                                                }
                                            }
                                            wait_animation_frame().await;
                                        }
                                    });
                                }

                                resume_raf();
                            });
                        }
                    }
                >
                    "Play"
                </button>
                <button
                    type="button"
                    class="wm-btn"
                    prop:disabled=move || !ready.get() || !playing.get()
                    on:click={
                        let stop_playback = Rc::clone(&stop_playback);
                        move |_| stop_playback()
                    }
                >
                    "Pause"
                </button>
                <button
                    type="button"
                    class=move || {
                        if looping.get() {
                            "wm-btn wm-btn-toggle is-on"
                        } else {
                            "wm-btn wm-btn-toggle"
                        }
                    }
                    prop:disabled=move || !ready.get()
                    prop:aria-pressed=move || looping.get()
                    on:click={
                        let player = Rc::clone(&player);
                        let sync_ui = Rc::clone(&sync_ui);
                        move |_| {
                            let next = !looping.get_untracked();
                            looping.set(next);
                            if let Ok(borrow) = player.try_borrow() {
                                if let Some(p) = borrow.as_ref() {
                                    p.set_loop(next);
                                }
                            }
                            sync_ui();
                        }
                    }
                >
                    "Loop"
                </button>
                <button
                    type="button"
                    class="wm-btn"
                    prop:disabled=move || busy.get() || !ready.get() || playing.get()
                    on:click={
                        let player = Rc::clone(&player);
                        let sync_ui = Rc::clone(&sync_ui);
                        move |_| step_frame(&player, &sync_ui, busy, error, playing, false)
                    }
                >
                    "Prev"
                </button>
                <button
                    type="button"
                    class="wm-btn"
                    prop:disabled=move || busy.get() || !ready.get() || playing.get()
                    on:click={
                        let player = Rc::clone(&player);
                        let sync_ui = Rc::clone(&sync_ui);
                        move |_| step_frame(&player, &sync_ui, busy, error, playing, true)
                    }
                >
                    "Next"
                </button>
                <input
                    type="number"
                    class="wm-frame-input"
                    min="0"
                    prop:value=move || frame.get().to_string()
                    prop:disabled=move || busy.get() || !ready.get() || playing.get()
                    on:change={
                        let player = Rc::clone(&player);
                        let sync_ui = Rc::clone(&sync_ui);
                        move |ev| {
                            let input: HtmlInputElement = event_target(&ev);
                            if let Ok(v) = input.value().parse::<u32>() {
                                seek_frame(&player, &sync_ui, busy, error, playing, v);
                            }
                        }
                    }
                />
                <button
                    type="button"
                    class="wm-btn"
                    prop:disabled=move || busy.get() || !ready.get() || playing.get()
                    on:click={
                        let player = Rc::clone(&player);
                        let sync_ui = Rc::clone(&sync_ui);
                        move |_| seek_frame(&player, &sync_ui, busy, error, playing, frame.get())
                    }
                >
                    "Seek"
                </button>
            </div>
            <p class="wm-status">
                {move || {
                    if busy.get() || !ready.get() {
                        status.get()
                    } else {
                        let fps_part = match fps.get() {
                            Some(v) => format!(" · {v:.1} fps"),
                            None => String::new(),
                        };
                        format!(
                            "frame {} / {} · {:.3}s{}{}{}",
                            frame.get(),
                            frame_count.get().saturating_sub(1),
                            timestamp.get(),
                            fps_part,
                            if playing.get() { " · playing" } else { "" },
                            if looping.get() { " · loop" } else { "" }
                        )
                    }
                }}
            </p>
            <p class="wm-pipeline">
                {move || {
                    let Some(s) = pipeline.get() else {
                        return String::new();
                    };
                    let range = match (s.presentation_from, s.presentation_to) {
                        (Some(a), Some(b)) if a == b => format!(" [{a}]"),
                        (Some(a), Some(b)) => format!(" [{a}–{b}]"),
                        _ => String::new(),
                    };
                    let lag = match s.last_behind_ms {
                        Some(ms) => format!(" · lag {ms:.0}ms"),
                        None => String::new(),
                    };
                    format!(
                        "prefetch {}/{}{} · held {} · decodeQ {} · skipped {}{}",
                        s.presentation_len,
                        s.presentation_cap,
                        range,
                        if s.held_len == usize::MAX {
                            "busy".into()
                        } else {
                            s.held_len.to_string()
                        },
                        if s.decode_queue_size == u32::MAX {
                            "busy".into()
                        } else {
                            s.decode_queue_size.to_string()
                        },
                        s.frames_skipped,
                        lag
                    )
                }}
            </p>
            <p class="wm-error">{move || error.get().unwrap_or_default()}</p>
        </div>
    }
}

fn format_metadata(meta: &VideoMetadata) -> String {
    let fps = meta
        .fps
        .map(|v| format!("{v:.3} fps"))
        .unwrap_or_else(|| "fps —".into());
    let codec = meta
        .codec
        .codec_string
        .clone()
        .unwrap_or_else(|| format!("{:?}", meta.codec.codec));
    format!(
        "{}×{} · {} · {} · {:.3}s · {} frames",
        meta.width,
        meta.height,
        fps,
        codec,
        meta.duration().seconds,
        meta.frame_count
    )
}

fn step_frame(
    player: &PlayerHandle,
    sync_ui: &Rc<dyn Fn()>,
    busy: RwSignal<bool>,
    error: RwSignal<Option<String>>,
    playing: RwSignal<bool>,
    forward: bool,
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
        let result = if forward {
            p.next_frame().await
        } else {
            p.previous_frame().await
        };
        if let Err(e) = &result {
            error.set(Some(e.to_string()));
        }
        *player.borrow_mut() = Some(p);
        sync_ui();
        busy.set(false);
    });
}

fn seek_frame(
    player: &PlayerHandle,
    sync_ui: &Rc<dyn Fn()>,
    busy: RwSignal<bool>,
    error: RwSignal<Option<String>>,
    playing: RwSignal<bool>,
    target: u32,
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
        if let Err(e) = p.seek_frame(u64::from(target)).await {
            error.set(Some(e.to_string()));
        }
        *player.borrow_mut() = Some(p);
        sync_ui();
        busy.set(false);
    });
}

fn event_target<T: JsCast>(ev: &leptos::ev::Event) -> T {
    ev.target()
        .expect("event target")
        .unchecked_into::<T>()
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

/// Await one animation frame — used by the async prefetch pump only.
/// Present pacing uses [`use_raf_fn_with_options`] instead.
async fn wait_animation_frame() {
    let promise = js_sys::Promise::new(&mut |resolve, _reject| {
        let window = web_sys::window().expect("window");
        let cb = wasm_bindgen::closure::Closure::once(move |_: f64| {
            let _ = resolve.call0(&wasm_bindgen::JsValue::NULL);
        });
        let _ = window.request_animation_frame(cb.as_ref().unchecked_ref());
        // Kept until the browser invokes it (once).
        cb.forget();
    });
    let _ = wasm_bindgen_futures::JsFuture::from(promise).await;
}

fn performance_now_ms() -> Option<f64> {
    web_sys::window()?.performance().map(|p| p.now())
}
