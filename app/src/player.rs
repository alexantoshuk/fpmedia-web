//! Leptos demo controls over [`webmedia::WebPlayer`].

use std::cell::RefCell;
use std::rc::Rc;

use js_sys::Uint8Array;
use leptos::html::Canvas;
use leptos::prelude::*;
use leptos::task::spawn_local;
use wasm_bindgen::JsCast;
use web_sys::{HtmlCanvasElement, HtmlInputElement};
use webmedia::WebPlayer;

type PlayerHandle = Rc<RefCell<Option<WebPlayer>>>;

#[component]
pub fn PlayerDemo() -> impl IntoView {
    let canvas_ref = NodeRef::<Canvas>::new();
    let player: PlayerHandle = Rc::new(RefCell::new(None));

    let busy = RwSignal::new(false);
    let error = RwSignal::new(Option::<String>::None);
    let frame = RwSignal::new(0_u32);
    let frame_count = RwSignal::new(0_u32);
    let timestamp = RwSignal::new(0.0_f64);
    let fps = RwSignal::new(0.0_f64);
    let playing = RwSignal::new(false);
    let ready = RwSignal::new(false);

    let sync_ui = {
        let player = Rc::clone(&player);
        Rc::new(move || {
            if let Some(p) = player.borrow().as_ref() {
                let st = p.state();
                frame.set(st.frame.min(u32::MAX as u64) as u32);
                frame_count.set(st.frame_count.min(u32::MAX as u64) as u32);
                timestamp.set(st.timestamp.seconds);
                fps.set(1.0 / p.frame_duration_seconds().max(1e-9));
                playing.set(st.playing);
            }
        }) as Rc<dyn Fn()>
    };

    let open_bytes = {
        let player = Rc::clone(&player);
        let sync_ui = Rc::clone(&sync_ui);
        Rc::new(move |bytes: Vec<u8>| {
            let Some(canvas_el) = canvas_ref.get() else {
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
                    class="wm-btn"
                    prop:disabled=move || busy.get()
                    on:click={
                        let open_bytes = Rc::clone(&open_bytes);
                        move |_| {
                            if busy.get_untracked() {
                                return;
                            }
                            error.set(None);
                            let open_bytes = Rc::clone(&open_bytes);
                            spawn_local(async move {
                                // Fetch without holding `busy` — `open_bytes` owns that flag.
                                match fetch_bytes("/short_gop.mp4").await {
                                    Ok(bytes) => open_bytes(bytes),
                                    Err(e) => error.set(Some(e)),
                                }
                            });
                        }
                    }
                >
                    "Load sample"
                </button>
                <button
                    type="button"
                    class="wm-btn wm-btn-primary"
                    prop:disabled=move || busy.get() || !ready.get() || playing.get()
                    on:click={
                        let player = Rc::clone(&player);
                        let sync_ui = Rc::clone(&sync_ui);
                        move |_| {
                            if busy.get_untracked() || !ready.get_untracked() || playing.get_untracked() {
                                return;
                            }
                            if let Some(p) = player.borrow_mut().as_mut() {
                                p.play();
                            }
                            playing.set(true);
                            let player = Rc::clone(&player);
                            let sync_ui = Rc::clone(&sync_ui);
                            spawn_local(async move {
                                loop {
                                    if !playing.get_untracked() {
                                        break;
                                    }
                                    let duration = {
                                        let borrow = player.borrow();
                                        match borrow.as_ref() {
                                            Some(p) => p.frame_duration_seconds(),
                                            None => break,
                                        }
                                    };
                                    sleep_ms(((duration * 1000.0).max(1.0)) as i32).await;
                                    if !playing.get_untracked() {
                                        break;
                                    }
                                    busy.set(true);
                                    let cont = {
                                        let mut borrow = player.borrow_mut();
                                        match borrow.as_mut() {
                                            Some(p) => match p.step_playback().await {
                                                Ok(more) => more,
                                                Err(e) => {
                                                    error.set(Some(e.to_string()));
                                                    false
                                                }
                                            },
                                            None => false,
                                        }
                                    };
                                    sync_ui();
                                    busy.set(false);
                                    if !cont {
                                        playing.set(false);
                                        break;
                                    }
                                }
                                if let Some(p) = player.borrow_mut().as_mut() {
                                    p.pause();
                                }
                                playing.set(false);
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
                            if !busy.get_untracked() {
                                if let Some(p) = player.borrow_mut().as_mut() {
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
                        format!(
                            "frame {} / {} · {:.3}s · {:.1} fps{}",
                            frame.get(),
                            frame_count.get().saturating_sub(1),
                            timestamp.get(),
                            fps.get(),
                            if playing.get() { " · playing" } else { "" }
                        )
                    }
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
        if let Some(p) = player.borrow_mut().as_mut() {
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
        if let Some(p) = player.borrow_mut().as_mut() {
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

async fn fetch_bytes(url: &str) -> Result<Vec<u8>, String> {
    let window = web_sys::window().ok_or("no window")?;
    let resp_value = wasm_bindgen_futures::JsFuture::from(window.fetch_with_str(url))
        .await
        .map_err(|e| format!("{e:?}"))?;
    let resp: web_sys::Response = resp_value.dyn_into().map_err(|_| "bad response")?;
    if !resp.ok() {
        return Err(format!("HTTP {}", resp.status()));
    }
    let buf = wasm_bindgen_futures::JsFuture::from(
        resp.array_buffer().map_err(|e| format!("{e:?}"))?,
    )
    .await
    .map_err(|e| format!("{e:?}"))?;
    Ok(Uint8Array::new(&buf).to_vec())
}

async fn sleep_ms(ms: i32) {
    let promise = js_sys::Promise::new(&mut |resolve, _reject| {
        let _ = web_sys::window()
            .expect("window")
            .set_timeout_with_callback_and_timeout_and_arguments_0(&resolve, ms);
    });
    let _ = wasm_bindgen_futures::JsFuture::from(promise).await;
}
