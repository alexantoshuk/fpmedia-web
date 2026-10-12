//! Demo controls over [`media_components::Video`] — not production player UI.

use leptos::ev;
use leptos::html::{Canvas, Div, Input};
use leptos::prelude::*;
use leptos_use::{UseTimeoutFnReturn, use_event_listener, use_timeout_fn};
use media_components::helpers::{
    format_metadata, format_timecode_at, odd_frame_px_runs, paint_strip_canvas_1px,
    playhead_width_css, strip_canvas_buffer_width,
};
use media_components::{
    HoverThumb, PipelineStats, ReadyFrameKind, Scrubber, ScrubberCtx, TimelineMeta,
    TimelineMetaKind, Video, VideoRef,
};
use wasm_bindgen::JsCast;
use web_sys::{Blob, Element, HtmlElement, HtmlInputElement, window};

/// Track zebra: odd frames darker than `.wm-scrubber-track` `#2a2a2a`
/// (~35% toward black — 20% was invisible under fetch/progress).
const FRAME_ZEBRA_ODD_COLOR: &str = "#1a1a1a";

/// Forward shuttle ladder for demo J/L keys (K → 1× + play/pause).
const JKL_STEPS: [f64; 4] = [1.0, 2.0, 4.0, 8.0];

/// Compact JSON for e2e (`data-testid="pipeline-json"`). Manual — app has no serde.
fn pipeline_stats_json(stats: Option<PipelineStats>) -> String {
    let Some(s) = stats else {
        return "{}".into();
    };
    let ranges = s
        .fetch_file_ranges
        .iter()
        .map(|(a, b)| format!("[{a:.6},{b:.6}]"))
        .collect::<Vec<_>>()
        .join(",");
    let ready = s
        .ready_frames
        .iter()
        .map(|m| {
            let kind = match m.kind {
                ReadyFrameKind::Keyframes => "Keyframes",
                ReadyFrameKind::Covered => "Covered",
                ReadyFrameKind::Exact => "Exact",
            };
            format!(
                "{{\"frame\":{},\"end\":{},\"kind\":\"{kind}\"}}",
                m.frame, m.end
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    let behind = match s.last_behind_ms {
        Some(ms) => format!("{ms:.3}"),
        None => "null".into(),
    };
    let held = if s.held_len == usize::MAX {
        "null".into()
    } else {
        s.held_len.to_string()
    };
    let dq = if s.decode_queue_size == u32::MAX {
        "null".into()
    } else {
        s.decode_queue_size.to_string()
    };
    let win_from = match s.scrub_window_from {
        Some(v) => v.to_string(),
        None => "null".into(),
    };
    let win_to = match s.scrub_window_to {
        Some(v) => v.to_string(),
        None => "null".into(),
    };
    format!(
        "{{\"fetch_file_ranges\":[{ranges}],\"ready_frames\":[{ready}],\"last_behind_ms\":{behind},\"underrun_holds\":{},\"underrun_hold_ms\":{:.3},\"frames_skipped\":{},\"held_len\":{held},\"decode_queue_size\":{dq},\"scrub_busy\":{},\"cache_frames\":{},\"cache_chunks\":{},\"cache_cap\":{},\"cache_no_gaps\":{},\"scrub_window_from\":{win_from},\"scrub_window_to\":{win_to},\"presentation_len\":{},\"presentation_cap\":{}}}",
        s.underrun_holds,
        s.underrun_hold_ms,
        s.frames_skipped,
        s.scrub_busy,
        s.cache_frames,
        s.cache_chunks,
        s.cache_cap,
        s.cache_no_gaps,
        s.presentation_len,
        s.presentation_cap,
    )
}

fn jkl_nearest_index(rate: f64) -> usize {
    let mut best = 0;
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

/// `dir`: −1 = J (slower), +1 = L (faster).
fn jkl_step(rate: f64, dir: i32) -> f64 {
    let i = jkl_nearest_index(rate);
    let next = (i as i32 + dir).clamp(0, (JKL_STEPS.len() - 1) as i32) as usize;
    JKL_STEPS[next]
}

#[component]
pub fn DemoPlayer() -> impl IntoView {
    let video = VideoRef::new();
    let url_input = NodeRef::<Input>::new();
    let player_root = NodeRef::<Div>::new();
    let fullscreen = RwSignal::new(false);
    // Fullscreen auto-hide of the control surface (not Google Chrome).
    let controls_visible = RwSignal::new(true);
    let over_controls = RwSignal::new(false);

    const CONTROLS_IDLE_MS: f64 = 2200.0;
    let UseTimeoutFnReturn {
        start: start_controls_hide,
        stop: stop_controls_hide,
        ..
    } = use_timeout_fn(
        move |_| {
            if fullscreen.get_untracked() && !over_controls.get_untracked() {
                controls_visible.set(false);
            }
        },
        CONTROLS_IDLE_MS,
    );

    let bump_controls = {
        let start_controls_hide = start_controls_hide.clone();
        let stop_controls_hide = stop_controls_hide.clone();
        move || {
            controls_visible.set(true);
            stop_controls_hide();
            if fullscreen.get_untracked() {
                start_controls_hide(());
            }
        }
    };

    // Keep signal in sync with Esc / browser UI exit.
    let _fs_change = use_event_listener(window(), ev::fullscreenchange, {
        let bump_controls = bump_controls.clone();
        let stop_controls_hide = stop_controls_hide.clone();
        move |_| {
            let active = window()
                .and_then(|w| w.document())
                .and_then(|d| d.fullscreen_element())
                .is_some();
            fullscreen.set(active);
            if active {
                bump_controls();
            } else {
                stop_controls_hide();
                controls_visible.set(true);
                over_controls.set(false);
            }
        }
    });

    let _fs_mousemove = use_event_listener(player_root, ev::mousemove, {
        let bump_controls = bump_controls.clone();
        move |_| {
            if fullscreen.get_untracked() {
                bump_controls();
            }
        }
    });

    // ← / → step when paused (pause if playing); Space play/pause; J/K/L rate.
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
                ev.prevent_default();
                if video.playing.get_untracked() {
                    video.pause();
                } else {
                    video.previous_frame();
                }
            }
            "ArrowRight" => {
                ev.prevent_default();
                if video.playing.get_untracked() {
                    video.pause();
                } else {
                    video.next_frame();
                }
            }
            " " | "Spacebar" => {
                ev.prevent_default();
                if video.playing.get_untracked() {
                    video.pause();
                } else {
                    video.play();
                }
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
                // Pause *before* snapping to 1× — `set_playback_rate(1)` while
                // playing spawns audio; doing that then pause leaves music on.
                if video.playing.get_untracked() {
                    video.pause();
                    video.set_playback_rate(1.0);
                } else {
                    video.set_playback_rate(1.0);
                    video.play();
                }
            }
            _ => {}
        }
    });

    view! {
        <div
            class=move || {
                if fullscreen.get() && !controls_visible.get() {
                    "wm-player is-controls-hidden"
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
                class="wm-player-ui"
                data-testid="player-ui"
                on:mouseenter=move |_| {
                    over_controls.set(true);
                    controls_visible.set(true);
                    stop_controls_hide();
                }
                on:mouseleave=move |_| {
                    over_controls.set(false);
                    if fullscreen.get_untracked() {
                        bump_controls();
                    }
                }
            >
                <DemoScrubber video=video/>
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
                        class=move || {
                            if video.playing.get() {
                                "wm-btn wm-btn-primary wm-btn-icon is-playing"
                            } else {
                                "wm-btn wm-btn-primary wm-btn-icon"
                            }
                        }
                        data-testid="btn-play-pause"
                        prop:disabled=move || video.busy.get() || !video.ready.get()
                        prop:aria-label=move || {
                            if video.playing.get() {
                                "Pause"
                            } else {
                                "Play"
                            }
                        }
                        prop:title=move || {
                            if video.playing.get() {
                                "Pause"
                            } else {
                                "Play"
                            }
                        }
                        on:click=move |_| {
                            if video.playing.get_untracked() {
                                video.pause();
                            } else {
                                video.play();
                            }
                        }
                    >
                        <svg
                            class="wm-icon wm-icon-play"
                            viewBox="0 0 24 24"
                            aria-hidden="true"
                            focusable="false"
                        >
                            <path d="M8 5v14l11-7z"></path>
                        </svg>
                        <svg
                            class="wm-icon wm-icon-pause"
                            viewBox="0 0 24 24"
                            aria-hidden="true"
                            focusable="false"
                        >
                            <rect x="6" y="5" width="4" height="14" rx="1"></rect>
                            <rect x="14" y="5" width="4" height="14" rx="1"></rect>
                        </svg>
                    </button>
                    <button
                        type="button"
                        class=move || {
                            if fullscreen.get() {
                                "wm-btn wm-btn-icon wm-btn-toggle is-on is-fullscreen"
                            } else {
                                "wm-btn wm-btn-icon"
                            }
                        }
                        data-testid="btn-fullscreen"
                        prop:aria-pressed=move || fullscreen.get()
                        prop:aria-label=move || {
                            if fullscreen.get() {
                                "Exit fullscreen"
                            } else {
                                "Fullscreen"
                            }
                        }
                        prop:title=move || {
                            if fullscreen.get() {
                                "Exit fullscreen"
                            } else {
                                "Fullscreen"
                            }
                        }
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
                        <svg
                            class="wm-icon wm-icon-fullscreen"
                            viewBox="0 0 24 24"
                            aria-hidden="true"
                            focusable="false"
                        >
                            <path d="M7 14H5v5h5v-2H7v-3zm-2-4h2V7h3V5H5v5zm12 7h-3v2h5v-5h-2v3zM14 5v2h3v3h2V5h-5z"></path>
                        </svg>
                        <svg
                            class="wm-icon wm-icon-fullscreen-exit"
                            viewBox="0 0 24 24"
                            aria-hidden="true"
                            focusable="false"
                        >
                            <path d="M5 16h3v3h2v-5H5v2zm3-8H5v2h5V5H8v3zm6 11h2v-3h3v-2h-5v5zm2-11V5h-2v5h5V8h-3z"></path>
                        </svg>
                    </button>
                    <button
                        type="button"
                        class=move || {
                            if video.looping.get() {
                                "wm-btn wm-btn-icon wm-btn-toggle is-on"
                            } else {
                                "wm-btn wm-btn-icon wm-btn-toggle"
                            }
                        }
                        data-testid="btn-loop"
                        prop:disabled=move || !video.ready.get()
                        prop:aria-pressed=move || video.looping.get()
                        prop:aria-label=move || {
                            if video.looping.get() {
                                "Loop on"
                            } else {
                                "Loop off"
                            }
                        }
                        prop:title=move || {
                            if video.looping.get() {
                                "Loop on"
                            } else {
                                "Loop off"
                            }
                        }
                        on:click=move |_| video.set_loop(!video.looping.get_untracked())
                    >
                        <svg
                            class="wm-icon wm-icon-loop"
                            viewBox="0 0 24 24"
                            aria-hidden="true"
                            focusable="false"
                        >
                            <path d="M7 7h10v3l4-4-4-4v3H5v6h2V7zm10 10H7v-3l-4 4 4 4v-3h12v-6h-2v4z"></path>
                        </svg>
                    </button>
                    <button
                        type="button"
                        class="wm-btn wm-btn-icon"
                        data-testid="btn-prev"
                        prop:disabled=move || video.busy.get() || !video.ready.get()
                        prop:aria-label="Previous frame"
                        prop:title="Previous frame"
                        on:click=move |_| {
                            if video.playing.get_untracked() {
                                video.pause();
                            } else {
                                video.previous_frame();
                            }
                        }
                    >
                        <svg
                            class="wm-icon wm-icon-prev"
                            viewBox="0 0 24 24"
                            aria-hidden="true"
                            focusable="false"
                        >
                            <path d="M14 6v12l-8.5-6L14 6z"></path>
                            <rect x="16" y="6" width="2.5" height="12" rx="0.5"></rect>
                        </svg>
                    </button>
                    <button
                        type="button"
                        class="wm-btn wm-btn-icon"
                        data-testid="btn-next"
                        prop:disabled=move || video.busy.get() || !video.ready.get()
                        prop:aria-label="Next frame"
                        prop:title="Next frame"
                        on:click=move |_| {
                            if video.playing.get_untracked() {
                                video.pause();
                            } else {
                                video.next_frame();
                            }
                        }
                    >
                        <svg
                            class="wm-icon wm-icon-next"
                            viewBox="0 0 24 24"
                            aria-hidden="true"
                            focusable="false"
                        >
                            <rect x="5.5" y="6" width="2.5" height="12" rx="0.5"></rect>
                            <path d="M10 6v12l8.5-6L10 6z"></path>
                        </svg>
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
                            let Some(input) = event_target::<HtmlInputElement>(&ev) else {
                                return;
                            };
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
                    <button
                        type="button"
                        class=move || {
                            if video.muted.get() {
                                "wm-btn wm-btn-icon wm-btn-toggle is-on is-muted"
                            } else {
                                "wm-btn wm-btn-icon wm-btn-toggle"
                            }
                        }
                        data-testid="btn-mute"
                        prop:disabled=move || !video.ready.get() || !video.has_audio.get()
                        prop:aria-pressed=move || video.muted.get()
                        prop:aria-label=move || {
                            if video.muted.get() {
                                "Unmute"
                            } else {
                                "Mute"
                            }
                        }
                        prop:title=move || {
                            if video.muted.get() {
                                "Unmute"
                            } else {
                                "Mute"
                            }
                        }
                        on:click=move |_| video.set_muted(!video.muted.get_untracked())
                    >
                        <svg
                            class="wm-icon wm-icon-volume"
                            viewBox="0 0 24 24"
                            aria-hidden="true"
                            focusable="false"
                        >
                            <path d="M3 10v4h4l5 5V5L7 10H3zm13.5 2c0-1.77-1.02-3.29-2.5-4.03v8.05c1.48-.73 2.5-2.25 2.5-4.02zM14 3.23v2.06c2.89.86 5 3.54 5 6.71s-2.11 5.85-5 6.71v2.06c4.01-.91 7-4.49 7-8.77s-2.99-7.86-7-8.77z"></path>
                        </svg>
                        <svg
                            class="wm-icon wm-icon-volume-mute"
                            viewBox="0 0 24 24"
                            aria-hidden="true"
                            focusable="false"
                        >
                            <path d="M16.5 12c0-1.77-1.02-3.29-2.5-4.03v2.21l2.45 2.45c.03-.2.05-.41.05-.63zm2.5 0c0 .94-.2 1.82-.54 2.64l1.51 1.51C20.63 14.91 21 13.5 21 12c0-4.28-2.99-7.86-7-8.77v2.06c2.89.86 5 3.54 5 6.71zM4.27 3 3 4.27 7.73 9H3v4h4l5 5v-6.73l4.25 4.25c-.67.52-1.42.93-2.25 1.18v2.06c1.38-.31 2.63-.95 3.69-1.81L19.73 21 21 19.73l-9-9L4.27 3zM12 4 9.91 6.09 12 8.18V4z"></path>
                        </svg>
                    </button>
                    <label
                        class="wm-volume"
                        data-testid="volume"
                        title="Volume"
                    >
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
                                let Some(input) = event_target::<HtmlInputElement>(&ev) else {
                                    return;
                                };
                                if let Ok(v) = input.value().parse::<f64>() {
                                    video.set_volume(v);
                                }
                            }
                        />
                    </label>
                    <label
                        class="wm-rate"
                        data-testid="playback-rate"
                        title="Playback rate (0.25–1.75 review; JKL 1/2/4/8; API up to 8×)"
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
                                let Some(input) = event_target::<HtmlInputElement>(&ev) else {
                                    return;
                                };
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
                                let Some(input) = event_target::<HtmlInputElement>(&ev) else {
                                    return;
                                };
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
                            placeholder="https://… (proxied) or blob:…"
                            data-testid="url-input"
                            prop:disabled=move || video.busy.get()
                            on:keydown=move |ev: leptos::ev::KeyboardEvent| {
                                if ev.key() != "Enter" {
                                    return;
                                }
                                ev.prevent_default();
                                let Some(input) = url_input.get() else { return };
                                // Empty → component sets sticky "empty URL" (keeps player).
                                video.open_url(input.value());
                            }
                        />
                        <button
                            type="button"
                            class="wm-btn"
                            data-testid="btn-open-url"
                            prop:disabled=move || video.busy.get()
                            on:click=move |_| {
                                let Some(input) = url_input.get() else { return };
                                // Empty → component sets sticky "empty URL" (keeps player).
                                video.open_url(input.value());
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
                            let tc_part = video
                                .metadata
                                .get()
                                .and_then(|m| format_timecode_at(&m, ph))
                                .map(|tc| format!(" · {tc}"))
                                .unwrap_or_default();
                            format!(
                                "{frame_part} / {}{tc_part} · {} · {:.3}s{}{}{}",
                                video.frame_count.get().saturating_sub(1),
                                video
                                    .metadata
                                    .get()
                                    .map(|m| {
                                        format!(
                                            "{} · play:{}",
                                            m.gop_profile.label(),
                                            m.play_policy().label()
                                        )
                                    })
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
                        let dense = if s.scrub_busy { " · dense" } else { "" };
                        format!(
                            "q {}/{} · held {} · decodeQ {} · skipped {}{}{}{}{}",
                            s.presentation_len,
                            s.presentation_cap,
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
                            dense,
                        )
                    }}
                </p>
                // Machine-readable PipelineStats for e2e zone targeting / latency samples.
                // Hidden; not production player UI.
                <pre
                    class="wm-pipeline-json"
                    data-testid="pipeline-json"
                    hidden
                    aria-hidden="true"
                >
                    {move || pipeline_stats_json(video.pipeline.get())}
                </pre>
                <p class="wm-error" data-testid="error">{move || video.error.get().unwrap_or_default()}</p>
            </div>
        </div>
    }
}

/// Odd-frame zebra on the progress track (demo chrome). Off when a frame is
/// sub-pixel after the same snap as strip greens.
#[component]
fn FrameZebra() -> impl IntoView {
    let ctx = use_context::<ScrubberCtx>().expect("FrameZebra must be used inside <Scrubber>");
    let video = ctx.video;
    let css_width = ctx.css_width;
    let canvas_ref = NodeRef::<Canvas>::new();

    Effect::new(move |_| {
        let count = video.frame_count.get();
        let css_w = css_width.get();
        let dpr = window()
            .map(|w| w.device_pixel_ratio())
            .unwrap_or(1.0)
            .max(1.0);
        let buf_w = strip_canvas_buffer_width(css_w, dpr);
        let Some(canvas) = canvas_ref.get() else {
            return;
        };
        let runs = odd_frame_px_runs(count, buf_w);
        let _ = paint_strip_canvas_1px(&canvas, buf_w, &runs, FRAME_ZEBRA_ODD_COLOR);
    });

    view! {
        <canvas class="wm-scrubber-zebra" node_ref=canvas_ref aria-hidden="true" />
    }
}

/// Demo scrubber chrome over headless [`Scrubber`] / [`TimelineMeta`] / [`HoverThumb`].
#[component]
fn DemoScrubber(video: VideoRef) -> impl IntoView {
    view! {
        <Scrubber video=video class="wm-scrubber".to_string()>
            <HoverThumb class="wm-thumb-preview".to_string() />
            <div class="wm-scrubber-track">
                <FrameZebra />
                <TimelineMeta
                    kind=TimelineMetaKind::Fetch
                    class="wm-scrubber-fetched".to_string()
                    test_id="scrubber-fetched"
                />
                <div
                    class="wm-scrubber-progress"
                    data-testid="scrubber-progress"
                    style:width=move || {
                        playhead_width_css(video.playhead.get(), video.frame_count.get())
                    }
                />
            </div>
            // Keyframes strip (yellow sync ticks) is debug-only — off by default
            // (distracting). Re-enable:
            //   <TimelineMeta kind=TimelineMetaKind::Keyframes class="wm-ready-strip is-keyframes".to_string() />
            // under Covered in the ready row.
            <div
                class="wm-scrubber-ready"
                aria-hidden="true"
                title="Decode densify: dim=markers (kept on Play), bright=blit ImageBitmap"
            >
                <TimelineMeta
                    kind=TimelineMetaKind::Covered
                    class="wm-ready-strip is-covered".to_string()
                />
                <TimelineMeta
                    kind=TimelineMetaKind::Exact
                    class="wm-ready-strip is-exact".to_string()
                />
            </div>
        </Scrubber>
    }
}

fn event_target<T: JsCast>(ev: &leptos::ev::Event) -> Option<T> {
    ev.target()?.dyn_into::<T>().ok()
}
