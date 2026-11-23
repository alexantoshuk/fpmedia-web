use super::controls::event_target;
use super::media_ops::DemoSession;
use fpmedia_web_components::helpers::{
    format_timecode_at_loose, format_timecode_from_fps, last_frame_id, parse_position,
};
use fpmedia_web_components::{
    GlobalFrame, HoverThumbCtx, VideoPlaylistRef, VideoRef, format_playlist_frames,
    format_playlist_goto_frames, format_playlist_goto_timecode, format_playlist_timecode,
    parse_playlist_goto,
};
use leptos::either::Either;
use leptos::html::{Button, Div, Input};
use leptos::prelude::*;
use web_sys::HtmlInputElement;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum PositionFormat {
    Frames,
    Timecode,
}

pub(super) fn format_position_side(video: &VideoRef, frame: u32, format: PositionFormat) -> String {
    match format {
        PositionFormat::Frames => frame.to_string(),
        PositionFormat::Timecode => match video.metadata.get() {
            Some(m) => format_timecode_at_loose(&m, frame),
            None => frame.to_string(),
        },
    }
}

pub(super) fn position_sides(video: &VideoRef, format: PositionFormat) -> (String, String) {
    let n = video.frame_count.get();
    if n == 0 {
        return ("—".into(), "—".into());
    }
    let cur = video.playhead.get().min(last_frame_id(n));
    let last = last_frame_id(n);
    (
        format_position_side(video, cur, format),
        format_position_side(video, last, format),
    )
}

pub(super) fn playlist_position_label(
    playlist: VideoPlaylistRef,
    format: PositionFormat,
) -> String {
    let (cur, last) = playlist_position_sides(playlist, format);
    format!("{cur} / {last}")
}

/// Playlist readout parts: `[i] now` and `end`.
///
/// While probing / opening (`!ready`), return a stable placeholder — live-slot
/// `frame_count` / metadata flicker through each probe open otherwise.
pub(super) fn playlist_position_sides(
    playlist: VideoPlaylistRef,
    format: PositionFormat,
) -> (String, String) {
    if !playlist.ready.get() {
        return ("—".into(), "—".into());
    }
    let pos = playlist.pos.get();
    let live = if playlist.live_is_a.get() {
        playlist.slot_a
    } else {
        playlist.slot_b
    };
    let n = live.frame_count.get().max(1);
    let last = last_frame_id(n);
    let i = pos.display_index();
    match format {
        PositionFormat::Frames => (format!("[{i}] {}", pos.frame.get()), last.to_string()),
        PositionFormat::Timecode => match live.metadata.get() {
            Some(m) => (
                format!("[{i}] {}", format_timecode_at_loose(&m, pos.frame.get())),
                format_timecode_at_loose(&m, last),
            ),
            // Live warm but meta not yet — keep Timecode mode via model fps.
            None => {
                let fps = playlist
                    .model
                    .get()
                    .clip(pos.clip_index as usize)
                    .and_then(|c| c.fps);
                (
                    format!("[{i}] {}", format_timecode_from_fps(pos.frame.get(), fps)),
                    format_timecode_from_fps(last, fps),
                )
            }
        },
    }
}

pub(super) fn playlist_meta_for_clip(
    playlist: VideoPlaylistRef,
    clip_index: u32,
) -> Option<fpmedia_web_components::VideoMetadata> {
    if playlist.live_clip.get_untracked() == clip_index {
        return playlist.live_video().metadata.get_untracked();
    }
    if playlist.idle_clip.get_untracked() == Some(clip_index) {
        return playlist.idle_video().metadata.get_untracked();
    }
    None
}

/// Frame.io-like center position: `current / last` (or `[i] now / end`), format menu, Go to… edit.
#[component]
pub(super) fn PositionDisplay(
    video: VideoRef,
    playlist: VideoPlaylistRef,
    session: RwSignal<DemoSession>,
    open: RwSignal<bool>,
    format: RwSignal<PositionFormat>,
    menu: NodeRef<Div>,
    trigger: NodeRef<Button>,
    rate_open: RwSignal<bool>,
    volume_open: RwSignal<bool>,
    settings_open: RwSignal<bool>,
) -> impl IntoView {
    let editing = RwSignal::new(false);
    let draft = RwSignal::new(String::new());
    let input_ref = NodeRef::<Input>::new();

    // Do **not** clear `editing` when `open` closes — Go to… closes the menu
    // then enters edit mode; a sync Effect would cancel it immediately.

    Effect::new(move |_| {
        if !editing.get() {
            return;
        }
        let Some(el) = input_ref.get() else {
            return;
        };
        let _ = el.focus();
        el.select();
    });

    let commit_goto = move || {
        if !editing.get_untracked() {
            return;
        }
        // Prefer live input value — controlled `draft` can lag behind the DOM.
        let raw = input_ref
            .get_untracked()
            .map(|el| el.value())
            .unwrap_or_else(|| draft.get_untracked());
        editing.set(false);
        open.set(false);
        if session.get_untracked().is_playlist() {
            if !playlist.ready.get_untracked() {
                return;
            }
            let cur = playlist.pos.get_untracked().clip_index;
            if let Some(aim) =
                parse_playlist_goto(&raw, cur, |i| playlist_meta_for_clip(playlist, i))
            {
                playlist.goto_pos(aim);
            }
            return;
        }
        if !video.ready.get_untracked() {
            return;
        }
        let Some(meta) = video.metadata.get_untracked() else {
            if let Ok(v) = raw.trim().parse::<u32>() {
                video.goto_frame(v);
            }
            return;
        };
        if let Some(frame) = parse_position(&meta, &raw) {
            video.goto_frame(frame);
        }
    };

    let cancel_goto = move || {
        editing.set(false);
        open.set(false);
    };

    let start_goto = move || {
        if session.get_untracked().is_playlist() {
            if !playlist.ready.get_untracked() {
                return;
            }
            // Editable `[i] now` only — `/ end` is display-only (product § Playlist position UI).
            let pos = playlist.pos.get_untracked();
            let draft_s = match format.get_untracked() {
                PositionFormat::Frames => format_playlist_goto_frames(pos),
                PositionFormat::Timecode => {
                    let live = playlist.live_video();
                    match live.metadata.get_untracked() {
                        Some(m) => format_playlist_goto_timecode(pos, &m),
                        None => {
                            let fps = playlist
                                .model
                                .get_untracked()
                                .clip(pos.clip_index as usize)
                                .and_then(|c| c.fps);
                            format!(
                                "[{}] {}",
                                pos.display_index(),
                                format_timecode_from_fps(pos.frame.get(), fps)
                            )
                        }
                    }
                }
            };
            draft.set(draft_s);
        } else {
            if !video.ready.get_untracked() {
                return;
            }
            let fmt = format.get_untracked();
            let n = video.frame_count.get_untracked();
            let cur = video.playhead.get_untracked().min(last_frame_id(n));
            draft.set(format_position_side(&video, cur, fmt));
        }
        rate_open.set(false);
        volume_open.set(false);
        settings_open.set(false);
        // Edit first, then close menu — never clear editing on menu close.
        editing.set(true);
        open.set(false);
    };

    view! {
        <div
            class=move || {
                if open.get() {
                    "wm-position wm-bar-menu-host is-open".to_string()
                } else {
                    "wm-position wm-bar-menu-host".to_string()
                }
            }
            data-testid="position"
        >
            // Scale container wraps readout only — not the menu (container-type
            // layout containment would trap abspos `.wm-bar-menu` like a second CB).
            <div class="wm-position-scale">
                {move || {
                    if editing.get() {
                        Either::Left(view! {
                            <input
                                type="text"
                                class="wm-position-input"
                                data-testid="frame-input"
                                node_ref=input_ref
                                prop:value=move || draft.get()
                                prop:aria-label="Go to"
                                on:input=move |ev| {
                                    let Some(input) = event_target::<HtmlInputElement>(&ev) else {
                                        return;
                                    };
                                    draft.set(input.value());
                                }
                                on:keydown=move |ev| {
                                    let key = ev.key();
                                    if key == "Enter" {
                                        ev.prevent_default();
                                        commit_goto();
                                    } else if key == "Escape" {
                                        ev.prevent_default();
                                        cancel_goto();
                                    }
                                }
                                on:blur=move |_| {
                                    commit_goto();
                                }
                            />
                        })
                    } else {
                        Either::Right(view! {
                            <button
                                type="button"
                                class="wm-position-display"
                                node_ref=trigger
                                data-testid="position-display"
                                prop:disabled=move || {
                                    if session.get().is_playlist() {
                                        !playlist.ready.get()
                                    } else {
                                        !video.ready.get()
                                    }
                                }
                                prop:aria-expanded=move || open.get()
                                prop:aria-haspopup="menu"
                                prop:aria-label=move || {
                                    if session.get().is_playlist() {
                                        playlist_position_label(playlist, format.get())
                                    } else {
                                        let (cur, last) = position_sides(&video, format.get());
                                        format!("{cur} / {last}")
                                    }
                                }
                                prop:title="Position (format / Go to…)"
                                on:click=move |_| {
                                    let ready = if session.get_untracked().is_playlist() {
                                        playlist.ready.get_untracked()
                                    } else {
                                        video.ready.get_untracked()
                                    };
                                    if !ready {
                                        return;
                                    }
                                    let next = !open.get_untracked();
                                    open.set(next);
                                    if next {
                                        rate_open.set(false);
                                        volume_open.set(false);
                                        settings_open.set(false);
                                    }
                                }
                            >
                                <Show
                                    when=move || session.get().is_playlist()
                                    fallback=move || {
                                        view! {
                                            <span class="wm-position-label">
                                                <span class="wm-position-cur">
                                                    {move || position_sides(&video, format.get()).0}
                                                </span>
                                                <span class="wm-position-slash" aria-hidden="true">
                                                    "/"
                                                </span>
                                                <span class="wm-position-rest">
                                                    {move || position_sides(&video, format.get()).1}
                                                </span>
                                            </span>
                                        }
                                    }
                                >
                                    <span
                                        class="wm-position-label"
                                        data-testid="playlist-position"
                                    >
                                        <span class="wm-position-cur">
                                            {move || playlist_position_sides(playlist, format.get()).0}
                                        </span>
                                        <span class="wm-position-slash" aria-hidden="true">
                                            "/"
                                        </span>
                                        <span class="wm-position-rest">
                                            {move || playlist_position_sides(playlist, format.get()).1}
                                        </span>
                                    </span>
                                </Show>
                            </button>
                        })
                    }
                }}
            </div>
            <div
                class="wm-bar-menu wm-bar-menu--center wm-position-menu"
                node_ref=menu
                role="menu"
                aria-label="Position format"
                data-testid="position-menu"
            >
                <button
                    type="button"
                    class=move || {
                        if format.get() == PositionFormat::Frames {
                            "wm-position-item-active"
                        } else {
                            "wm-position-item"
                        }
                    }
                    role="menuitemradio"
                    prop:aria-checked=move || format.get() == PositionFormat::Frames
                    data-testid="position-format-frames"
                    on:click=move |ev| {
                        ev.stop_propagation();
                        format.set(PositionFormat::Frames);
                        open.set(false);
                    }
                >
                    <span class="wm-position-check" aria-hidden="true">
                        {move || {
                            if format.get() == PositionFormat::Frames {
                                "✓"
                            } else {
                                ""
                            }
                        }}
                    </span>
                    "Frames"
                </button>
                <button
                    type="button"
                    class=move || {
                        if format.get() == PositionFormat::Timecode {
                            "wm-position-item-active"
                        } else {
                            "wm-position-item"
                        }
                    }
                    role="menuitemradio"
                    prop:aria-checked=move || format.get() == PositionFormat::Timecode
                    data-testid="position-format-timecode"
                    on:click=move |ev| {
                        ev.stop_propagation();
                        format.set(PositionFormat::Timecode);
                        open.set(false);
                    }
                >
                    <span class="wm-position-check" aria-hidden="true">
                        {move || {
                            if format.get() == PositionFormat::Timecode {
                                "✓"
                            } else {
                                ""
                            }
                        }}
                    </span>
                    "Timecode"
                </button>
                <div class="wm-position-menu-sep" role="separator"></div>
                <button
                    type="button"
                    class="wm-position-item wm-position-goto"
                    role="menuitem"
                    data-testid="position-goto"
                    on:click=move |ev| {
                        ev.stop_propagation();
                        start_goto();
                    }
                >
                    <span class="wm-position-check" aria-hidden="true"></span>
                    "Go to…"
                </button>
            </div>
        </div>
    }
}

/// Hover aim under the thumb — same Frames/Timecode format as position menu.
#[component]
pub(super) fn HoverThumbFrameLabel(
    video: VideoRef,
    format: RwSignal<PositionFormat>,
) -> impl IntoView {
    let ctx = use_context::<HoverThumbCtx>().expect("HoverThumbFrameLabel under HoverThumb");
    view! {
        <div class="wm-thumb-frame" data-testid="scrubber-thumb-frame">
            {move || {
                ctx.frame
                    .get()
                    .map(|f| format_position_side(&video, f, format.get()))
                    .unwrap_or_default()
            }}
        </div>
    }
}

/// Playlist hover unit is global frame — show `[i] local` like the position readout.
#[component]
pub(super) fn HoverPlaylistThumbLabel(
    playlist: VideoPlaylistRef,
    format: RwSignal<PositionFormat>,
) -> impl IntoView {
    let ctx = use_context::<HoverThumbCtx>().expect("HoverPlaylistThumbLabel under HoverThumb");
    view! {
        <div class="wm-thumb-frame" data-testid="scrubber-thumb-frame">
            {move || {
                let Some(global) = ctx.frame.get() else {
                    return String::new();
                };
                let model = playlist.model.get();
                let Some(pos) = model.at_global_frame(GlobalFrame::new(global)) else {
                    return String::new();
                };
                let Some(clip) = model.clip(pos.clip_index as usize) else {
                    return String::new();
                };
                let n = clip.frame_count.max(1);
                // Warm-slot metadata only when that slot owns this clip — never
                // borrow live meta for an off-slot hover (wrong fps / TC start).
                let meta = {
                    let live_i = playlist.live_clip.get();
                    let idle_i = playlist.idle_clip.get();
                    let live_is_a = playlist.live_is_a.get();
                    if live_i == pos.clip_index {
                        let live = if live_is_a {
                            playlist.slot_a
                        } else {
                            playlist.slot_b
                        };
                        live.metadata.get()
                    } else if idle_i == Some(pos.clip_index) {
                        let idle = if live_is_a {
                            playlist.slot_b
                        } else {
                            playlist.slot_a
                        };
                        idle.metadata.get()
                    } else {
                        None
                    }
                };
                match format.get() {
                    PositionFormat::Frames => format_playlist_frames(pos, n),
                    PositionFormat::Timecode => match meta {
                        Some(m) => format_playlist_timecode(pos, &m),
                        // Probe timing from the model — Timecode mode must not
                        // fall back to bare frames when the slot is cold.
                        None => {
                            let last = last_frame_id(n);
                            let now = format_timecode_from_fps(pos.frame.get(), clip.fps);
                            let end = format_timecode_from_fps(last, clip.fps);
                            format!("[{}] {now} / {end}", pos.display_index())
                        }
                    },
                }
            }}
        </div>
    }
}
