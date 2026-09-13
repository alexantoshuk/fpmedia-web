//! Demo controls over [`video::Video`] — not production chrome.

use leptos::ev;
use leptos::prelude::*;
use leptos_use::use_event_listener;
use wasm_bindgen::JsCast;
use web_sys::{Blob, Element, HtmlInputElement, window};
use video::{ReadyFrameKind, Video, VideoMetadata, VideoRef};

#[component]
pub fn DemoPlayer() -> impl IntoView {
    let video = VideoRef::new();

    // ← / → match Prev / Next buttons (skip when typing in an input).
    let _arrow_keys = use_event_listener(window(), ev::keydown, move |ev| {
        let Some(target) = ev.target() else {
            return;
        };
        if let Ok(el) = target.dyn_into::<Element>() {
            let tag = el.tag_name().to_ascii_lowercase();
            if tag == "input" || tag == "textarea" || tag == "select" {
                return;
            }
        }
        let can_step = video.ready.get_untracked()
            && !video.busy.get_untracked()
            && !video.playing.get_untracked();
        if !can_step {
            return;
        }
        match ev.key().as_str() {
            "ArrowLeft" => {
                ev.prevent_default();
                video.previous_frame();
            }
            "ArrowRight" => {
                ev.prevent_default();
                video.next_frame();
            }
            _ => {}
        }
    });

    view! {
        <div class="wm-player">
            <div class="wm-media">
                <Video video=video/>
                <Scrubber video=video/>
            </div>
            <BufferMeters video=video/>
            <p class="wm-meta">
                {move || video.metadata.get().as_ref().map(format_metadata).unwrap_or_default()}
            </p>
            <div class="wm-controls">
                <label class="wm-file">
                    "Open MP4"
                    <input
                        type="file"
                        accept="video/mp4,video/*"
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
                <button
                    type="button"
                    class="wm-btn wm-btn-primary"
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
                    prop:disabled=move || !video.ready.get() || !video.playing.get()
                    on:click=move |_| video.pause()
                >
                    "Pause"
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
                    prop:disabled=move || !video.ready.get()
                    prop:aria-pressed=move || video.looping.get()
                    on:click=move |_| video.set_loop(!video.looping.get_untracked())
                >
                    "Loop"
                </button>
                <button
                    type="button"
                    class="wm-btn"
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
                    min="0"
                    prop:value=move || video.frame.get().to_string()
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
                    prop:disabled=move || {
                        video.busy.get() || !video.ready.get() || video.playing.get()
                    }
                    on:click=move |_| video.seek_frame(video.frame.get())
                >
                    "Seek"
                </button>
            </div>
            <p class="wm-status">
                {move || {
                    if video.busy.get() || !video.ready.get() {
                        video.status.get()
                    } else {
                        let fps_part = match video.fps.get() {
                            Some(v) => format!(" · {v:.1} fps"),
                            None => String::new(),
                        };
                        format!(
                            "frame {} / {} · {} · {:.3}s{}{}{}",
                            video.frame.get(),
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
            <p class="wm-pipeline">
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
                    format!(
                        "held {} · decodeQ {} · skipped {}{}",
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
                        format!("{range}{lag}")
                    )
                }}
            </p>
            <p class="wm-error">{move || video.error.get().unwrap_or_default()}</p>
        </div>
    }
}

/// Timeline scrubber under the video — red = current / drag preview frame.
///
/// Drag is **async**: cancelable exact scrubs (latest wins), bar follows the
/// pointer immediately. Prev/Next stay **sync** (queue + `busy`).
/// Preview stays pinned until the engine playhead catches up so release does not
/// flash through intermediate committed frames.
#[component]
fn Scrubber(video: VideoRef) -> impl IntoView {
    let scrubbing = RwSignal::new(false);
    // Optimistic playhead while dragging (and until engine matches after release).
    let preview_frame = RwSignal::new(None::<u32>);

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

    let display_frame = Signal::derive(move || {
        preview_frame
            .get()
            .unwrap_or_else(|| video.frame.get())
    });

    // Drop optimistic preview once the committed playhead reaches it.
    Effect::new(move |_| {
        let Some(preview) = preview_frame.get() else {
            return;
        };
        if scrubbing.get() {
            return;
        }
        if video.frame.get() == preview {
            preview_frame.set(None);
        }
    });

    view! {
        <div
            class="wm-scrubber"
            role="slider"
            prop:aria-valuemin="0"
            prop:aria-valuemax=move || video.frame_count.get().saturating_sub(1)
            prop:aria-valuenow=move || display_frame.get()
            prop:aria-disabled=move || !video.ready.get()
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
                if let Some(frame) = frame_from_client_x(&el, ev.client_x() as f64) {
                    preview_frame.set(Some(frame));
                    video.scrub_frame(frame);
                }
            }
            on:pointermove=move |ev| {
                if !scrubbing.get_untracked() {
                    return;
                }
                let Ok(el) = ev.current_target().unwrap().dyn_into::<Element>() else {
                    return;
                };
                if let Some(frame) = frame_from_client_x(&el, ev.client_x() as f64) {
                    preview_frame.set(Some(frame));
                    video.scrub_frame(frame);
                }
            }
            on:pointerup=move |_| {
                scrubbing.set(false);
                if let Some(frame) = preview_frame.get_untracked() {
                    if video.frame.get_untracked() != frame {
                        video.scrub_frame(frame);
                    } else {
                        preview_frame.set(None);
                    }
                }
            }
            on:pointercancel=move |_| {
                scrubbing.set(false);
                preview_frame.set(None);
            }
        >
            <div class="wm-scrubber-track">
                <div
                    class="wm-scrubber-progress"
                    style:width=move || {
                        let max = video.frame_count.get().saturating_sub(1).max(1);
                        let pct = (display_frame.get() as f64 / max as f64) * 100.0;
                        format!("{pct:.3}%")
                    }
                />
            </div>
            <div
                class="wm-scrubber-ready"
                aria-hidden="true"
                title="Ready frames: green=exact, blue=ahead"
            >
                {move || {
                    let marks = video
                        .pipeline
                        .get()
                        .map(|s| s.ready_frames)
                        .unwrap_or_default();
                    let max = video.frame_count.get().saturating_sub(1).max(1) as f64;
                    marks
                        .into_iter()
                        .map(|m| {
                            let pct = (m.frame as f64 / max) * 100.0;
                            let class = match m.kind {
                                ReadyFrameKind::Exact => "wm-ready-tick is-exact",
                                ReadyFrameKind::Ahead => "wm-ready-tick is-ahead",
                            };
                            view! {
                                <span
                                    class=class
                                    style:left=format!("{pct:.3}%")
                                    title=format!("frame {}", m.frame)
                                />
                            }
                        })
                        .collect_view()
                }}
            </div>
        </div>
    }
}

/// Separate buffer meters under the scrubber (not overlaid on the timeline).
#[component]
fn BufferMeters(video: VideoRef) -> impl IntoView {
    view! {
        <div class="wm-buffer-meters">
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
            <MeterRow
                label="Gap fill"
                detail=Signal::derive(move || {
                    let Some(s) = video.pipeline.get() else {
                        return String::new();
                    };
                    let gap = s
                        .scrub_gap_target
                        .map(|g| format!("gap {g}"))
                        .unwrap_or_else(|| "gap —".into());
                    format!(
                        "aim {} · {gap} · {} · {}st {}µs · {} · idle {}",
                        s.scrub_aim,
                        s.scrub_fill_stop,
                        s.scrub_gap_steps,
                        s.scrub_gap_us,
                        s.scrub_gap_path,
                        s.scrub_fill_idle_passes,
                    )
                })
                fill_pct=Signal::derive(move || {
                    let Some(s) = video.pipeline.get() else {
                        return 0.0;
                    };
                    if s.scrub_gap_saturated {
                        return 100.0;
                    }
                    if s.scrub_busy {
                        return 60.0;
                    }
                    if s.scrub_gap_armed {
                        return 30.0;
                    }
                    0.0
                })
            />
        </div>
    }
}

#[component]
fn MeterRow(
    label: &'static str,
    detail: Signal<String>,
    fill_pct: Signal<f64>,
) -> impl IntoView {
    view! {
        <div class="wm-meter">
            <div class="wm-meter-label">
                <span>{label}</span>
                <span class="wm-meter-detail">{move || detail.get()}</span>
            </div>
            <div class="wm-meter-track">
                <div
                    class="wm-meter-fill"
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
    ev.target()
        .expect("event target")
        .unchecked_into::<T>()
}
