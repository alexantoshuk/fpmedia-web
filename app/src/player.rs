//! Demo controls over [`video::Video`] — not production chrome.

use leptos::ev;
use leptos::html::{Div, Input};
use leptos::prelude::*;
use leptos_use::{UseTimeoutFnReturn, use_event_listener, use_timeout_fn};
use video::{ReadyFrameKind, Video, VideoMetadata, VideoRef};
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::spawn_local;
use web_sys::{Blob, Element, HtmlElement, HtmlInputElement, window};

/// Frame.io JKL shuttle ladder (J walks left, L walks right, K → 1×).
const JKL_STEPS: [f64; 8] = [-8.0, -4.0, -2.0, -1.0, 1.0, 2.0, 4.0, 8.0];

fn jkl_nearest_index(rate: f64) -> usize {
    let mut best = 4; // 1.0
    let mut best_dist = f64::INFINITY;
    for (i, &step) in JKL_STEPS.iter().enumerate() {
        let d = (rate - step).abs();
        if d < best_dist {
            best_dist = d;
            best = i;
        }
    }
    best
}

/// `dir`: −1 = J (slower forward / more reverse), +1 = L.
fn jkl_step(rate: f64, dir: i32) -> f64 {
    let i = jkl_nearest_index(rate);
    let next = (i as i32 + dir).clamp(0, (JKL_STEPS.len() - 1) as i32) as usize;
    JKL_STEPS[next]
}

/// Coalesce scrub engine work after the Leptos/tachys event callback releases its
/// `RefCell`. Duplicate targets (left-edge clamp spams `frame=0`) collapse to one
/// pending sample so microtasks do not pile up and re-enter cache inserts.
/// `settle` = pointer up (exact-first); drag keeps densify on.
fn defer_scrub(video: VideoRef, frame: u32, settle: bool) {
    thread_local! {
        static PENDING: std::cell::Cell<Option<(u32, bool)>> =
            const { std::cell::Cell::new(None) };
        static SCHEDULED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    }
    PENDING.with(|p| p.set(Some((frame, settle))));
    if SCHEDULED.with(|s| s.get()) {
        return;
    }
    SCHEDULED.with(|s| s.set(true));
    spawn_local(async move {
        while let Some((frame, settle)) = PENDING.with(|p| p.take()) {
            if settle {
                video.scrub_settle(frame);
            } else {
                video.scrub_frame(frame);
            }
            if PENDING.with(|p| p.get().is_none()) {
                break;
            }
        }
        SCHEDULED.with(|s| s.set(false));
        if let Some((frame, settle)) = PENDING.with(|p| p.get()) {
            defer_scrub(video, frame, settle);
        }
    });
}

#[component]
pub fn DemoPlayer() -> impl IntoView {
    let video = VideoRef::new();
    let url_input = NodeRef::<Input>::new();
    let player_root = NodeRef::<Div>::new();
    let fullscreen = RwSignal::new(false);
    let chrome_visible = RwSignal::new(true);
    let over_chrome = RwSignal::new(false);

    const CHROME_IDLE_MS: f64 = 2200.0;
    let UseTimeoutFnReturn {
        start: start_chrome_hide,
        stop: stop_chrome_hide,
        ..
    } = use_timeout_fn(
        move |_| {
            if fullscreen.get_untracked() && !over_chrome.get_untracked() {
                chrome_visible.set(false);
            }
        },
        CHROME_IDLE_MS,
    );

    let bump_chrome = {
        let start_chrome_hide = start_chrome_hide.clone();
        let stop_chrome_hide = stop_chrome_hide.clone();
        move || {
            chrome_visible.set(true);
            stop_chrome_hide();
            if fullscreen.get_untracked() {
                start_chrome_hide(());
            }
        }
    };

    // Keep signal in sync with Esc / browser UI exit.
    let _fs_change = use_event_listener(window(), ev::fullscreenchange, {
        let bump_chrome = bump_chrome.clone();
        let stop_chrome_hide = stop_chrome_hide.clone();
        move |_| {
            let active = window()
                .and_then(|w| w.document())
                .and_then(|d| d.fullscreen_element())
                .is_some();
            fullscreen.set(active);
            if active {
                bump_chrome();
            } else {
                stop_chrome_hide();
                chrome_visible.set(true);
                over_chrome.set(false);
            }
        }
    });

    let _fs_mousemove = use_event_listener(player_root, ev::mousemove, {
        let bump_chrome = bump_chrome.clone();
        move |_| {
            if fullscreen.get_untracked() {
                bump_chrome();
            }
        }
    });

    // ← / → step when paused; J/K/L shuttle. Skip when typing.
    let _transport_keys = use_event_listener(window(), ev::keydown, move |ev| {
        let Some(target) = ev.target() else {
            return;
        };
        if let Ok(el) = target.dyn_into::<Element>() {
            let tag = el.tag_name().to_ascii_lowercase();
            if tag == "input" || tag == "textarea" || tag == "select" {
                return;
            }
        }
        if !video.ready.get_untracked() {
            return;
        }
        match ev.key().as_str() {
            "ArrowLeft" => {
                if video.playing.get_untracked() {
                    return;
                }
                ev.prevent_default();
                video.previous_frame();
            }
            "ArrowRight" => {
                if video.playing.get_untracked() {
                    return;
                }
                ev.prevent_default();
                video.next_frame();
            }
            "l" | "L" => {
                ev.prevent_default();
                let next = jkl_step(video.playback_rate.get_untracked(), 1);
                video.set_playback_rate(next);
                if !video.playing.get_untracked() {
                    video.play();
                }
            }
            "j" | "J" => {
                ev.prevent_default();
                let next = jkl_step(video.playback_rate.get_untracked(), -1);
                video.set_playback_rate(next);
                if !video.playing.get_untracked() {
                    video.play();
                }
            }
            "k" | "K" => {
                ev.prevent_default();
                video.set_playback_rate(1.0);
                if video.playing.get_untracked() {
                    video.pause();
                } else {
                    video.play();
                }
            }
            _ => {}
        }
    });

    view! {
        <div
            class=move || {
                if fullscreen.get() && !chrome_visible.get() {
                    "wm-player is-chrome-hidden"
                } else {
                    "wm-player"
                }
            }
            data-testid="player"
            node_ref=player_root
        >
            <div class="wm-media">
                <Video video=video/>
            </div>
            <div
                class="wm-chrome"
                data-testid="chrome"
                on:mouseenter=move |_| {
                    over_chrome.set(true);
                    chrome_visible.set(true);
                    stop_chrome_hide();
                }
                on:mouseleave=move |_| {
                    over_chrome.set(false);
                    if fullscreen.get_untracked() {
                        bump_chrome();
                    }
                }
            >
                <Scrubber video=video/>
                <BufferMeters video=video/>
                <p class="wm-meta" data-testid="meta">
                    {move || {
                        let mut s = video
                            .metadata
                            .get()
                            .as_ref()
                            .map(format_metadata)
                            .unwrap_or_default();
                        if video.has_audio.get() {
                            if !s.is_empty() {
                                s.push_str(" · ");
                            }
                            s.push_str("audio");
                        }
                        s
                    }}
                </p>
                <div class="wm-controls" data-testid="controls">
                    <button
                        type="button"
                        class="wm-btn wm-btn-primary"
                        data-testid="btn-play"
                        prop:disabled=move || {
                            video.busy.get() || !video.ready.get() || video.playing.get()
                        }
                        on:click=move |_| video.play()
                    >
                        "Play"
                    </button>
                    <button
                        type="button"
                        class="wm-btn"
                        data-testid="btn-pause"
                        prop:disabled=move || !video.ready.get() || !video.playing.get()
                        on:click=move |_| video.pause()
                    >
                        "Pause"
                    </button>
                    <button
                        type="button"
                        class=move || {
                            if fullscreen.get() {
                                "wm-btn wm-btn-toggle is-on"
                            } else {
                                "wm-btn"
                            }
                        }
                        data-testid="btn-fullscreen"
                        prop:aria-pressed=move || fullscreen.get()
                        on:click=move |_| {
                            let Some(root) = player_root.get_untracked() else {
                                return;
                            };
                            let Ok(el) = root.dyn_into::<HtmlElement>() else {
                                return;
                            };
                            let Some(doc) = window().and_then(|w| w.document()) else {
                                return;
                            };
                            if fullscreen.get_untracked() {
                                doc.exit_fullscreen();
                            } else {
                                let _ = el.request_fullscreen();
                            }
                        }
                    >
                        {move || {
                            if fullscreen.get() {
                                "Exit full"
                            } else {
                                "Full"
                            }
                        }}
                    </button>
                    <button
                        type="button"
                        class=move || {
                            if video.looping.get() {
                                "wm-btn wm-btn-toggle is-on"
                            } else {
                                "wm-btn wm-btn-toggle"
                            }
                        }
                        data-testid="btn-loop"
                        prop:disabled=move || !video.ready.get()
                        prop:aria-pressed=move || video.looping.get()
                        on:click=move |_| video.set_loop(!video.looping.get_untracked())
                    >
                        "Loop"
                    </button>
                    <button
                        type="button"
                        class="wm-btn"
                        data-testid="btn-prev"
                        prop:disabled=move || {
                            video.busy.get() || !video.ready.get() || video.playing.get()
                        }
                        on:click=move |_| video.previous_frame()
                    >
                        "Prev"
                    </button>
                    <button
                        type="button"
                        class="wm-btn"
                        data-testid="btn-next"
                        prop:disabled=move || {
                            video.busy.get() || !video.ready.get() || video.playing.get()
                        }
                        on:click=move |_| video.next_frame()
                    >
                        "Next"
                    </button>
                    <input
                        type="number"
                        class="wm-frame-input"
                        data-testid="frame-input"
                        min="0"
                        prop:value=move || video.playhead.get().to_string()
                        prop:disabled=move || {
                            video.busy.get() || !video.ready.get() || video.playing.get()
                        }
                        on:change=move |ev| {
                            let input: HtmlInputElement = event_target(&ev);
                            if let Ok(v) = input.value().parse::<u32>() {
                                video.seek_frame(v);
                            }
                        }
                    />
                    <button
                        type="button"
                        class="wm-btn"
                        data-testid="btn-seek"
                        prop:disabled=move || {
                            video.busy.get() || !video.ready.get() || video.playing.get()
                        }
                        on:click=move |_| video.seek_frame(video.playhead.get())
                    >
                        "Seek"
                    </button>
                </div>
                <div class="wm-controls wm-audio" data-testid="audio-controls">
                    <button
                        type="button"
                        class=move || {
                            if video.muted.get() {
                                "wm-btn wm-btn-toggle is-on"
                            } else {
                                "wm-btn wm-btn-toggle"
                            }
                        }
                        data-testid="btn-mute"
                        prop:disabled=move || !video.ready.get() || !video.has_audio.get()
                        prop:aria-pressed=move || video.muted.get()
                        on:click=move |_| video.set_muted(!video.muted.get_untracked())
                    >
                        {move || if video.muted.get() { "Unmute" } else { "Mute" }}
                    </button>
                    <label
                        class="wm-volume"
                        data-testid="volume"
                        title="Volume"
                    >
                        <span class="wm-volume-label">"Vol"</span>
                        <input
                            type="range"
                            class="wm-volume-input"
                            data-testid="volume-input"
                            min="0"
                            max="1"
                            step="0.01"
                            prop:value=move || video.volume.get().to_string()
                            prop:disabled=move || !video.ready.get() || !video.has_audio.get()
                            on:input=move |ev| {
                                let input: HtmlInputElement = event_target(&ev);
                                if let Ok(v) = input.value().parse::<f64>() {
                                    video.set_volume(v);
                                }
                            }
                        />
                    </label>
                    <label
                        class="wm-rate"
                        data-testid="playback-rate"
                        title="Playback rate (0.25–1.75 review; J/L = −8…−1 / 1 / 2…8; K = 1×)"
                    >
                        <span class="wm-rate-label">
                            {move || format!("{:.2}x", video.playback_rate.get())}
                        </span>
                        <input
                            type="range"
                            class="wm-rate-input"
                            data-testid="playback-rate-input"
                            min="0.25"
                            max="1.75"
                            step="0.05"
                            prop:value=move || {
                                let r = video.playback_rate.get();
                                if r < 0.0 {
                                    "0.25".to_string()
                                } else {
                                    r.min(1.75).to_string()
                                }
                            }
                            prop:disabled=move || !video.ready.get()
                            on:input=move |ev| {
                                let input: HtmlInputElement = event_target(&ev);
                                if let Ok(v) = input.value().parse::<f64>() {
                                    video.set_playback_rate(v);
                                }
                            }
                        />
                    </label>
                </div>
                <div class="wm-controls wm-source" data-testid="source">
                    <label class="wm-file">
                        "Browse"
                        <input
                            type="file"
                            accept="video/mp4,video/*"
                            data-testid="file-input"
                            on:change=move |ev: leptos::ev::Event| {
                                let input: HtmlInputElement = event_target(&ev);
                                let Some(files) = input.files() else { return };
                                let Some(file) = files.get(0) else { return };
                                let name = file.name();
                                video.status.set(format!("Selected {name}…"));
                                // File is a Blob — progressive open, no whole-file array_buffer.
                                let blob: Blob = file.unchecked_into();
                                video.open_blob(blob);
                            }
                        />
                    </label>
                    <div class="wm-url-row">
                        <input
                            type="url"
                            class="wm-url-input"
                            node_ref=url_input
                            placeholder="https://… or blob:…"
                            data-testid="url-input"
                            prop:disabled=move || video.busy.get()
                            on:keydown=move |ev: leptos::ev::KeyboardEvent| {
                                if ev.key() != "Enter" {
                                    return;
                                }
                                ev.prevent_default();
                                let Some(input) = url_input.get() else { return };
                                let url = input.value();
                                if url.trim().is_empty() {
                                    return;
                                }
                                video.open_url(url);
                            }
                        />
                        <button
                            type="button"
                            class="wm-btn"
                            data-testid="btn-open-url"
                            prop:disabled=move || video.busy.get()
                            on:click=move |_| {
                                let Some(input) = url_input.get() else { return };
                                let url = input.value();
                                if url.trim().is_empty() {
                                    return;
                                }
                                video.open_url(url);
                            }
                        >
                            "Open URL"
                        </button>
                    </div>
                </div>
                <p class="wm-status" data-testid="status">
                    {move || {
                        if video.busy.get() || !video.ready.get() {
                            video.status.get()
                        } else {
                            let fps_part = match video.fps.get() {
                                Some(v) => format!(" · {v:.1} fps"),
                                None => String::new(),
                            };
                            let ph = video.playhead.get();
                            let eng = video.frame.get();
                            let frame_part = if ph == eng {
                                format!("frame {ph}")
                            } else {
                                format!("frame {ph} (eng {eng})")
                            };
                            format!(
                                "{frame_part} / {} · {} · {:.3}s{}{}{}",
                                video.frame_count.get().saturating_sub(1),
                                video
                                    .metadata
                                    .get()
                                    .map(|m| m.gop_profile.label())
                                    .unwrap_or_else(|| "—".into()),
                                video.timestamp.get(),
                                fps_part,
                                if video.playing.get() { " · playing" } else { "" },
                                if video.looping.get() { " · loop" } else { "" }
                            )
                        }
                    }}
                </p>
                <p class="wm-pipeline" data-testid="pipeline">
                    {move || {
                        let Some(s) = video.pipeline.get() else {
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
                        let holds = if s.underrun_holds > 0 || s.underrun_hold_ms > 0.0 {
                            format!(
                                " · holds {}/{:.0}ms",
                                s.underrun_holds, s.underrun_hold_ms
                            )
                        } else {
                            String::new()
                        };
                        format!(
                            "held {} · decodeQ {} · skipped {}{}{}{}",
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
                            range,
                            lag,
                            holds,
                        )
                    }}
                </p>
                <p class="wm-error" data-testid="error">{move || video.error.get().unwrap_or_default()}</p>
            </div>
        </div>
    }
}

/// Timeline scrubber — bar follows [`VideoRef::playhead`] (potential frame).
///
/// Scrub updates playhead immediately and decodes async without latching `busy`.
/// Prev/Next/Seek/Play/Pause also drive `playhead` and commit via sync ops.
///
/// Build a hard-stop `linear-gradient` for coalesced cache runs (O(1) DOM).
fn cache_strip_gradient(
    marks: &[video::ReadyFrameMark],
    kind: ReadyFrameKind,
    max: f64,
    color: &str,
) -> String {
    let max = max.max(1.0);
    let mut stops = String::from("linear-gradient(90deg, transparent 0%");
    let mut any = false;
    for m in marks.iter().filter(|m| m.kind == kind) {
        any = true;
        let start = (m.frame as f64 / max) * 100.0;
        let end = ((m.end.max(m.frame) as f64 + 1.0) / max) * 100.0;
        let start = start.clamp(0.0, 100.0);
        let end = end.clamp(0.0, 100.0).max(start);
        stops.push_str(&format!(
            ", transparent {start:.3}%, {color} {start:.3}%, {color} {end:.3}%, transparent {end:.3}%"
        ));
    }
    if !any {
        return "none".into();
    }
    stops.push_str(", transparent 100%)");
    stops
}

#[component]
fn Scrubber(video: VideoRef) -> impl IntoView {
    let scrubbing = RwSignal::new(false);

    let frame_from_client_x = move |el: &Element, client_x: f64| -> Option<u32> {
        if !video.ready.get_untracked() {
            return None;
        }
        let rect = el.get_bounding_client_rect();
        let w = rect.width().max(1.0);
        let x = (client_x - rect.left()).clamp(0.0, w);
        let max_frame = video.frame_count.get_untracked().saturating_sub(1);
        let frame = ((x / w) * max_frame as f64).round() as u32;
        Some(frame.min(max_frame))
    };

    view! {
        <div
            class="wm-scrubber"
            data-testid="scrubber"
            role="slider"
            attr:aria-valuemin="0"
            attr:aria-valuemax=move || video.frame_count.get().saturating_sub(1).to_string()
            attr:aria-valuenow=move || video.playhead.get().to_string()
            attr:aria-disabled=move || (!video.ready.get()).to_string()
            data-playhead=move || video.playhead.get().to_string()
            on:pointerdown=move |ev| {
                if !video.ready.get_untracked() {
                    return;
                }
                let Ok(el) = ev.current_target().unwrap().dyn_into::<Element>() else {
                    return;
                };
                if let Ok(html) = el.clone().dyn_into::<web_sys::HtmlElement>() {
                    let _ = html.set_pointer_capture(ev.pointer_id());
                }
                scrubbing.set(true);
                if let Some(frame) = frame_from_client_x(&el, ev.client_x()) {
                    defer_scrub(video, frame, false);
                }
            }
            on:pointermove=move |ev| {
                if !scrubbing.get_untracked() {
                    return;
                }
                let Ok(el) = ev.current_target().unwrap().dyn_into::<Element>() else {
                    return;
                };
                if let Some(frame) = frame_from_client_x(&el, ev.client_x()) {
                    defer_scrub(video, frame, false);
                }
            }
            on:pointerup=move |_| {
                scrubbing.set(false);
                let frame = video.playhead.get_untracked();
                defer_scrub(video, frame, true);
            }
            on:pointercancel=move |_| {
                scrubbing.set(false);
            }
        >
            <div class="wm-scrubber-track">
                <div
                    class="wm-scrubber-progress"
                    data-testid="scrubber-progress"
                    style:width=move || {
                        let max = video.frame_count.get().saturating_sub(1).max(1);
                        let pct = (video.playhead.get() as f64 / max as f64) * 100.0;
                        format!("{pct:.3}%")
                    }
                />
            </div>
            <div
                class="wm-scrubber-ready"
                aria-hidden="true"
                title="Cache strips: dim=markers (kept on Play), bright=blit ImageBitmap"
            >
                {move || {
                    let marks = video
                        .pipeline
                        .get()
                        .map(|s| s.ready_frames)
                        .unwrap_or_default();
                    let max = video.frame_count.get().saturating_sub(1).max(1) as f64;
                    let covered = cache_strip_gradient(&marks, ReadyFrameKind::Covered, max, "#34d399");
                    let exact = cache_strip_gradient(&marks, ReadyFrameKind::Exact, max, "#34d399");
                    view! {
                        <div
                            class="wm-ready-strip is-covered"
                            style:background-image=covered
                        />
                        <div
                            class="wm-ready-strip is-exact"
                            style:background-image=exact
                        />
                    }
                }}
            </div>
        </div>
    }
}

/// Separate buffer meters under the scrubber (not overlaid on the timeline).
#[component]
fn BufferMeters(video: VideoRef) -> impl IntoView {
    view! {
        <div class="wm-buffer-meters" data-testid="buffer-meters">
            <MeterRow
                label="Prefetch queue"
                detail=Signal::derive(move || {
                    let Some(s) = video.pipeline.get() else {
                        return String::new();
                    };
                    format!("{}/{}", s.presentation_len, s.presentation_cap)
                })
                fill_pct=Signal::derive(move || {
                    let Some(s) = video.pipeline.get() else {
                        return 0.0;
                    };
                    if s.presentation_cap == 0 {
                        return 0.0;
                    }
                    (s.presentation_len as f64 / s.presentation_cap as f64) * 100.0
                })
            />
            <MeterRow
                label="Prefetch ahead"
                detail=Signal::derive(move || {
                    let Some(s) = video.pipeline.get() else {
                        return String::new();
                    };
                    match (s.presentation_from, s.presentation_to) {
                        (Some(a), Some(b)) if a == b => format!("frame {a}"),
                        (Some(a), Some(b)) => format!("{a}–{b}"),
                        _ => "—".into(),
                    }
                })
                fill_pct=Signal::derive(move || {
                    let max = video.frame_count.get().saturating_sub(1).max(1) as f64;
                    let Some(s) = video.pipeline.get() else {
                        return 0.0;
                    };
                    let tip = s
                        .presentation_to
                        .unwrap_or(video.frame.get() as u64)
                        .max(video.frame.get() as u64);
                    (tip as f64 / max) * 100.0
                })
            />
            <MeterRow
                label="Frame cache"
                detail=Signal::derive(move || {
                    let Some(s) = video.pipeline.get() else {
                        return String::new();
                    };
                    let phase = if s.scrub_busy { " · dense" } else { "" };
                    let solid = if s.cache_no_gaps { " · no_gaps" } else { "" };
                    let win = match (s.scrub_window_from, s.scrub_window_to) {
                        (Some(a), Some(b)) => format!(" · win {a}–{b}"),
                        _ => String::new(),
                    };
                    format!("{}f / {}c{phase}{solid}{win}", s.cache_frames, s.cache_chunks)
                })
                fill_pct=Signal::derive(move || {
                    let Some(s) = video.pipeline.get() else {
                        return 0.0;
                    };
                    let cap = s.cache_cap.max(1) as f64;
                    ((s.cache_frames as f64 / cap) * 100.0).min(100.0)
                })
            />
        </div>
    }
}

#[component]
fn MeterRow(label: &'static str, detail: Signal<String>, fill_pct: Signal<f64>) -> impl IntoView {
    let test_id = match label {
        "Prefetch queue" => "meter-prefetch-queue",
        "Prefetch ahead" => "meter-prefetch-ahead",
        "Frame cache" => "meter-frame-cache",
        _ => "meter",
    };
    view! {
        <div class="wm-meter" data-testid=test_id>
            <div class="wm-meter-label">
                <span>{label}</span>
                <span class="wm-meter-detail" data-testid=format!("{test_id}-detail")>
                    {move || detail.get()}
                </span>
            </div>
            <div class="wm-meter-track">
                <div
                    class="wm-meter-fill"
                    data-testid=format!("{test_id}-fill")
                    style:width=move || format!("{:.3}%", fill_pct.get())
                />
            </div>
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

fn event_target<T: JsCast>(ev: &leptos::ev::Event) -> T {
    ev.target().expect("event target").unchecked_into::<T>()
}
