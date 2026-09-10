//! Demo controls over [`video::Video`] — not production chrome.

use leptos::prelude::*;
use wasm_bindgen::JsCast;
use web_sys::{Blob, HtmlInputElement};
use video::{Video, VideoMetadata, VideoRef};

#[component]
pub fn DemoPlayer() -> impl IntoView {
    let video = VideoRef::new();

    view! {
        <div class="wm-player">
            <Video video=video/>
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
                            "frame {} / {} · {:.3}s{}{}{}",
                            video.frame.get(),
                            video.frame_count.get().saturating_sub(1),
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
            <p class="wm-error">{move || video.error.get().unwrap_or_default()}</p>
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
