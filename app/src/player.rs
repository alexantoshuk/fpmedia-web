//! Leptos demo controls over [`webmedia::WebPlayer`].

use std::cell::RefCell;
use std::rc::Rc;

use js_sys::Uint8Array;
use leptos::html::Canvas;
use leptos::prelude::*;
use leptos::task::spawn_local;
use wasm_bindgen::JsCast;
use web_sys::{HtmlCanvasElement, HtmlInputElement};
use webmedia::{DuePresent, PipelineStats, WebPlayer};

type PlayerHandle = Rc<RefCell<Option<WebPlayer>>>;

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
    let player: PlayerHandle = Rc::new(RefCell::new(None));
    let fps_meter = Rc::new(RefCell::new(FpsMeter::new()));

    let busy = RwSignal::new(false);
    let error = RwSignal::new(Option::<String>::None);
    let frame = RwSignal::new(0_u32);
    let frame_count = RwSignal::new(0_u32);
    let timestamp = RwSignal::new(0.0_f64);
    // Measured display FPS while playing (`None` when idle / not enough samples).
    let fps = RwSignal::new(Option::<f64>::None);
    let pipeline = RwSignal::new(Option::<PipelineStats>::None);
    let playing = RwSignal::new(false);
    let looping = RwSignal::new(false);
    let ready = RwSignal::new(false);

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

    let open_bytes = {
        let player = Rc::clone(&player);
        let sync_ui = Rc::clone(&sync_ui);
        let fps_meter = Rc::clone(&fps_meter);
        Rc::new(move |bytes: Vec<u8>| {
            let Some(canvas_el) = canvas_ref.get_untracked() else {
                error.set(Some("canvas not mounted".into()));
                return;
            };
            let canvas: HtmlCanvasElement = canvas_el;
            if busy.get_untracked() {
                return;
            }
            busy.set(true);
            error.set(None);
            playing.set(false);
            looping.set(false);
            fps.set(None);
            pipeline.set(None);
            fps_meter.borrow_mut().reset();
            let player = Rc::clone(&player);
            let sync_ui = Rc::clone(&sync_ui);
            spawn_local(async move {
                *player.borrow_mut() = None;
                match WebPlayer::open_mp4_bytes(bytes, canvas).await {
                    Ok(p) => {
                        *player.borrow_mut() = Some(p);
                        ready.set(true);
                        sync_ui();
                    }
                    Err(e) => {
                        ready.set(false);
                        error.set(Some(e.to_string()));
                    }
                }
                busy.set(false);
            });
        }) as Rc<dyn Fn(Vec<u8>)>
    };

    view! {
        <div class="wm-player">
            <canvas
                node_ref=canvas_ref
                class="wm-canvas"
                width="320"
                height="240"
            />
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
                        let sync_ui = Rc::clone(&sync_ui);
                        let fps_meter = Rc::clone(&fps_meter);
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
                            let sync_ui = Rc::clone(&sync_ui);
                            let fps_meter = Rc::clone(&fps_meter);
                            spawn_local(async move {
                                // Mediabunny-style: AudioContext clock + rAF present,
                                // with decode on a *separate* task so presents never
                                // wait on WebCodecs.
                                let audio = match web_sys::AudioContext::new() {
                                    Ok(ctx) => {
                                        if let Ok(p) = ctx.resume() {
                                            let _ = wasm_bindgen_futures::JsFuture::from(p).await;
                                        }
                                        Some(ctx)
                                    }
                                    Err(_) => None,
                                };

                                let mut media_origin = {
                                    let borrow = player.borrow();
                                    match borrow.as_ref() {
                                        Some(p) => p.state().timestamp.seconds,
                                        None => {
                                            playing.set(false);
                                            return;
                                        }
                                    }
                                };
                                let mut audio_origin =
                                    audio.as_ref().map(|a| a.current_time()).unwrap_or(0.0);
                                let mut wall_origin =
                                    performance_now_ms().map(|t| t / 1000.0).unwrap_or(0.0);

                                {
                                    let borrow = player.borrow();
                                    if let Some(p) = borrow.as_ref() {
                                        if let Err(e) = p.prime_playback().await {
                                            error.set(Some(e.to_string()));
                                            playing.set(false);
                                            return;
                                        }
                                    } else {
                                        playing.set(false);
                                        return;
                                    }
                                }

                                // Background decode pump.
                                {
                                    let player = Rc::clone(&player);
                                    spawn_local(async move {
                                        while playing.get_untracked() {
                                            let need = {
                                                let borrow = player.borrow();
                                                borrow.as_ref().is_some_and(|p| {
                                                    p.is_playing()
                                                        && !p.is_restarting()
                                                        && p.presentation_len() <= 4
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
                                                    Ok(true) => continue, // keep filling
                                                    Ok(false) => {
                                                        // EOF / full / paused — must yield or we spin.
                                                    }
                                                    Err(e) => {
                                                        error.set(Some(e.to_string()));
                                                        playing.set(false);
                                                        break;
                                                    }
                                                }
                                            }
                                            wait_animation_frame().await;
                                        }
                                    });
                                }

                                let media_time = |media_origin: f64,
                                                  audio_origin: f64,
                                                  wall_origin: f64|
                                 -> f64 {
                                    if let Some(a) = audio.as_ref() {
                                        a.current_time() - audio_origin + media_origin
                                    } else if let Some(now) = performance_now_ms() {
                                        now / 1000.0 - wall_origin + media_origin
                                    } else {
                                        media_origin
                                    }
                                };

                                let mut last_ui_ms = performance_now_ms().unwrap_or(0.0);
                                loop {
                                    if !playing.get_untracked() {
                                        break;
                                    }
                                    wait_animation_frame().await;
                                    if !playing.get_untracked() {
                                        break;
                                    }

                                    {
                                        let want = looping.get_untracked();
                                        if let Ok(borrow) = player.try_borrow() {
                                            if let Some(p) = borrow.as_ref() {
                                                p.set_loop(want);
                                            }
                                        }
                                    }

                                    let t = media_time(media_origin, audio_origin, wall_origin);
                                    let outcome = {
                                        let borrow = player.borrow();
                                        match borrow.as_ref() {
                                            Some(p) => p.present_due(t),
                                            None => break,
                                        }
                                    };

                                    match outcome {
                                        Ok(DuePresent::Ended) => {
                                            playing.set(false);
                                            break;
                                        }
                                        Ok(DuePresent::NeedsLoop) => {
                                            // Drop the RefCell borrow before awaiting restart.
                                            let restarted = {
                                                let borrow = player.borrow();
                                                match borrow.as_ref() {
                                                    Some(p) => Some(p.loop_to_start().await),
                                                    None => None,
                                                }
                                            };
                                            match restarted {
                                                Some(Ok(())) => {
                                                    media_origin = 0.0;
                                                    audio_origin = audio
                                                        .as_ref()
                                                        .map(|a| a.current_time())
                                                        .unwrap_or(0.0);
                                                    wall_origin = performance_now_ms()
                                                        .map(|n| n / 1000.0)
                                                        .unwrap_or(0.0);
                                                }
                                                Some(Err(e)) => {
                                                    error.set(Some(e.to_string()));
                                                    playing.set(false);
                                                    break;
                                                }
                                                None => break,
                                            }
                                            continue;
                                        }
                                        Ok(DuePresent::Playing { did_draw }) => {
                                            if did_draw {
                                                if let Some(measured) =
                                                    fps_meter.borrow_mut().note_present()
                                                {
                                                    let now_ms = performance_now_ms();
                                                    if now_ms
                                                        .is_some_and(|n| n - last_ui_ms >= 100.0)
                                                    {
                                                        fps.set(Some(measured));
                                                    }
                                                }
                                            }
                                        }
                                        Err(e) => {
                                            error.set(Some(e.to_string()));
                                            playing.set(false);
                                            break;
                                        }
                                    }

                                    if let Some(now) = performance_now_ms() {
                                        if now - last_ui_ms >= 100.0 {
                                            sync_ui();
                                            last_ui_ms = now;
                                        }
                                    }
                                }
                                if let Ok(borrow) = player.try_borrow() {
                                    if let Some(p) = borrow.as_ref() {
                                        p.pause();
                                    }
                                }
                                playing.set(false);
                                fps_meter.borrow_mut().reset();
                                fps.set(None);
                                sync_ui();
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
                        let player = Rc::clone(&player);
                        move |_| {
                            playing.set(false);
                            if let Ok(borrow) = player.try_borrow() {
                                if let Some(p) = borrow.as_ref() {
                                    p.pause();
                                }
                            }
                        }
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
                    if !ready.get() {
                        "Load an MP4 to begin.".to_string()
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
                        s.held_len,
                        s.decode_queue_size,
                        s.frames_skipped,
                        lag
                    )
                }}
            </p>
            <p class="wm-error">{move || error.get().unwrap_or_default()}</p>
        </div>
    }
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
        if let Some(p) = player.borrow().as_ref() {
            p.pause();
            let result = if forward {
                p.next_frame().await
            } else {
                p.previous_frame().await
            };
            if let Err(e) = result {
                error.set(Some(e.to_string()));
            }
        }
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
        if let Some(p) = player.borrow().as_ref() {
            p.pause();
            if let Err(e) = p.seek_frame(u64::from(target)).await {
                error.set(Some(e.to_string()));
            }
        }
        sync_ui();
        busy.set(false);
    });
}

fn event_target<T: JsCast>(ev: &leptos::ev::Event) -> T {
    ev.target()
        .expect("event target")
        .unchecked_into::<T>()
}

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
