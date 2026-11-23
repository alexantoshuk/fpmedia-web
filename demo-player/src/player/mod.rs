//! Demo controls over [`fpmedia_web_components::Video`] / [`VideoPlaylist`] — not production player UI.

mod controls;
mod format;
mod lines;
mod media_ops;
mod open;
mod position;
mod scrubbers;
mod zebra;

use self::controls::{
    CONTROLS_IDLE_MS, PlaybackRateTrack, VolumeRangeInput, event_target, try_lock_fs_landscape,
    try_unlock_orientation,
};
use self::format::{format_fps_label, jkl_step, rate_is_unity};
use self::lines::{
    demo_meta_line, demo_pipeline_line, demo_stats_clipboard, demo_status_line, pipeline_stats_json,
};
use self::media_ops::{
    DemoSession, demo_mark_loop_in, demo_mark_loop_out, demo_media, demo_media_untracked,
    demo_next_frame, demo_previous_frame, demo_reset_loop_zone, demo_source_fps, demo_toggle_loop,
    demo_toggle_play,
};
use self::open::{open_demo_files, open_demo_urls};
use self::position::{PositionDisplay, PositionFormat};
use self::scrubbers::{DemoPlaylistScrubber, DemoScrubber};
use crate::icon;
use fpmedia_web_components::{
    PipelinePlane, ScrubCachePolicy, ThumbPolicy, VERSION, Video, VideoPlaylist, VideoPlaylistRef,
    VideoRef,
};
use leptos::either::EitherOf3;
use leptos::ev;
use leptos::html::{Button, Div, Input};
use leptos::prelude::*;
use leptos_use::{
    OnClickOutsideOptions, UseClipboardReturn, UseTimeoutFnReturn, on_click_outside,
    on_click_outside_with_options, use_clipboard, use_event_listener, use_media_query,
    use_timeout_fn,
};
use wasm_bindgen::JsCast;
use web_sys::{Element, HtmlElement, HtmlInputElement, window};

#[component]
pub fn DemoPlayer() -> impl IntoView {
    let video = VideoRef::new();
    let playlist = VideoPlaylistRef::new();
    // Last open: single clip vs ≥2 files/URLs — mounts one host via `<Show>`.
    let session = RwSignal::new(DemoSession::Single);
    // Debug pipeline line + `pipeline-json` e2e — lease meters on the live paint slot.
    Effect::new(move |_| {
        let active = demo_media(video, playlist, session).active_video();
        let lease = active.lease_pipeline_plane(PipelinePlane::Meters);
        on_cleanup(move || drop(lease));
    });
    let player_root = NodeRef::<Div>::new();
    let fullscreen = RwSignal::new(false);
    // Fullscreen auto-hide of the control surface (not Google Chrome).
    let controls_visible = RwSignal::new(true);
    let over_controls = RwSignal::new(false);
    // Demo A/B (debug panel): hide timeline chrome / drop thumb sink.
    let scrubber_on = RwSignal::new(true);
    let thumbs_on = RwSignal::new(true);
    // Hover / Pause-progressive decode box (square, px); starts at the host default.
    let thumb_box = RwSignal::new(video.thumb_max_size().0);
    // Touch-primary (phone/tablet): no hover UI → no ThumbSink / progressive.
    // Laptops with a touchscreen usually stay `(hover: hover)` via trackpad/mouse.
    let no_hover = use_media_query("(hover: none)");
    let debug_on = RwSignal::new(false);
    // Rate / volume / position / settings popovers.
    let rate_open = RwSignal::new(false);
    let volume_open = RwSignal::new(false);
    let position_open = RwSignal::new(false);
    let settings_open = RwSignal::new(false);
    let position_format = RwSignal::new(PositionFormat::Frames);
    // Keep V volume pop open while dragging past the panel.
    let volume_sliding = RwSignal::new(false);
    let rate_root = NodeRef::<Div>::new();
    let volume_root = NodeRef::<Div>::new();
    // Bar menus: outside-target = panel (not the wide host). Trigger ignored so toggle still works.
    let position_menu = NodeRef::<Div>::new();
    let position_trigger = NodeRef::<Button>::new();
    let settings_menu = NodeRef::<Div>::new();
    let settings_trigger = NodeRef::<Button>::new();
    // File name or URL string from last open (clip line under the toolbar).
    let clip_label = RwSignal::new(None::<String>);
    let url_input = NodeRef::<Input>::new();
    let UseClipboardReturn {
        copied: stats_copied,
        copy: copy_stats,
        ..
    } = use_clipboard();

    let media_ready = Memo::new(move |_| demo_media(video, playlist, session).ready.get());
    // Playlist: facade intent — not live slot (Host seam hard-stop gap flashes Play).
    let media_playing = Memo::new(move |_| demo_media(video, playlist, session).playing.get());
    let media_looping = Memo::new(move |_| demo_media(video, playlist, session).looping.get());

    Effect::new(move |_| {
        let policy = if scrubber_on.get() {
            ScrubCachePolicy::On
        } else {
            ScrubCachePolicy::Off
        };
        if session.get().is_playlist() {
            let _ = playlist.live_is_a.get();
            // Playlist owns VT Covered — Off wipes session markers (not live slot ledger).
            playlist.set_scrub_cache_policy(policy);
        } else {
            video.set_scrub_cache_policy(policy);
        }
    });
    Effect::new(move |_| {
        let on = scrubber_on.get() && thumbs_on.get() && !no_hover.get();
        let policy = if on {
            ThumbPolicy::On
        } else {
            ThumbPolicy::Off
        };
        if session.get().is_playlist() {
            // Both slots: idle keeps ThumbSink for neighbor / hover across cuts.
            playlist.slot_a.set_thumb_policy(policy);
            playlist.slot_b.set_thumb_policy(policy);
        } else {
            video.set_thumb_policy(policy);
        }
    });
    // Active media follows the debug size select (re-applies when single↔playlist flips).
    Effect::new(move |_| {
        let n = thumb_box.get();
        demo_media(video, playlist, session).set_thumb_max_size(n, n);
    });

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
                // Phone/tablet: lock landscape after FS is active (needs FS context).
                if no_hover.get_untracked() {
                    try_lock_fs_landscape();
                }
                bump_controls();
            } else {
                if no_hover.get_untracked() {
                    try_unlock_orientation();
                }
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
        if !media_ready.get_untracked() {
            return;
        }
        let playing = media_playing.get_untracked();
        let media = demo_media_untracked(video, playlist, session);
        match ev.key().as_str() {
            "ArrowLeft" => {
                ev.prevent_default();
                demo_previous_frame(media);
            }
            "ArrowRight" => {
                ev.prevent_default();
                demo_next_frame(media);
            }
            " " | "Spacebar" => {
                ev.prevent_default();
                demo_toggle_play(media, playing);
            }
            "l" | "L" => {
                ev.prevent_default();
                let next = jkl_step(media.playback_rate.get_untracked(), 1);
                media.set_playback_rate(next);
                if !playing {
                    media.play();
                }
            }
            "j" | "J" => {
                ev.prevent_default();
                let next = jkl_step(media.playback_rate.get_untracked(), -1);
                media.set_playback_rate(next);
                if !playing {
                    media.play();
                }
            }
            "k" | "K" => {
                ev.prevent_default();
                // Pause *before* snapping to 1× — `set_playback_rate(1)` while
                // playing spawns audio; doing that then pause leaves music on.
                if playing {
                    media.pause();
                    media.set_playback_rate(1.0);
                } else {
                    media.set_playback_rate(1.0);
                    media.play();
                }
            }
            "Escape" => {
                rate_open.set(false);
                volume_open.set(false);
                position_open.set(false);
                settings_open.set(false);
            }
            "[" => {
                ev.prevent_default();
                demo_mark_loop_in(media);
            }
            "]" => {
                ev.prevent_default();
                demo_mark_loop_out(media);
            }
            _ => {}
        }
    });

    let _rate_outside = on_click_outside(rate_root, move |_| {
        rate_open.set(false);
    });
    let _volume_outside = on_click_outside(volume_root, move |_| {
        volume_open.set(false);
    });
    let _position_outside = on_click_outside_with_options(
        position_menu,
        move |_| {
            // Menu only — Go to edit keeps `open=false` and must not be cancelled here.
            position_open.set(false);
        },
        OnClickOutsideOptions::default().ignore(position_trigger),
    );
    let _settings_outside = on_click_outside_with_options(
        settings_menu,
        move |_| {
            settings_open.set(false);
        },
        OnClickOutsideOptions::default().ignore(settings_trigger),
    );

    // Text buttons: filled bg, no border. Icon buttons: icon-only (no chrome).
    let btn = "inline-flex items-center justify-center rounded-md border-0 bg-zinc-800 px-3 py-2 text-sm text-zinc-100 hover:bg-zinc-700 disabled:cursor-default disabled:opacity-45";
    let btn_icon = "wm-btn-icon inline-flex h-9 w-9 items-center justify-center rounded-md border-0 bg-transparent text-zinc-100 hover:bg-zinc-800/60 disabled:cursor-default disabled:opacity-45";
    // Accent only when the glyph itself does not show state (Loop / menu open /
    // copied). Play / mute / fullscreen swap icons — stay zinc.
    let btn_icon_on = "wm-btn-icon inline-flex h-9 w-9 items-center justify-center rounded-md border-0 bg-transparent text-emerald-400 hover:bg-zinc-800/60 disabled:cursor-default disabled:opacity-45";
    let btn_play = "wm-btn-icon inline-flex h-9 w-9 items-center justify-center rounded-md border-0 bg-transparent text-zinc-100 hover:bg-zinc-800/60 disabled:cursor-default disabled:opacity-45";
    // Step buttons: no bare `inline-flex` — it fights `@max-*`/`hidden` and leaves a gap.
    // Hidden until player container ≥ `@3xl` (~768px); then no layout space when gone.
    let btn_icon_step = "wm-btn-icon hidden @3xl:inline-flex h-9 w-9 items-center justify-center rounded-md border-0 bg-transparent text-zinc-100 hover:bg-zinc-800/60 disabled:cursor-default disabled:opacity-45";
    // Mono + size like position/timecode default (`wm-position-field` clamp max).
    // `min-w-16` (~4rem) reserves `XX.Xfps` / `1.50x` so the bar does not jump.
    // Accent when rate ≠ 1× (fps label stays zinc).
    let btn_rate = "wm-rate-btn inline-flex h-9 min-w-16 items-center justify-center rounded-md border-0 bg-transparent px-1.5 font-mono text-[0.8125rem] tabular-nums text-zinc-300 hover:text-zinc-100 disabled:cursor-default disabled:opacity-45";
    let btn_rate_accent = "wm-rate-btn inline-flex h-9 min-w-16 items-center justify-center rounded-md border-0 bg-transparent px-1.5 font-mono text-[0.8125rem] tabular-nums text-[var(--wm-accent)] hover:text-[var(--wm-accent)] disabled:cursor-default disabled:opacity-45";

    let btn_round = "inline-flex h-7 w-7 items-center justify-center rounded-full border-0 bg-transparent text-zinc-100 opacity-50 hover:bg-zinc-800/60 hover:opacity-100 disabled:cursor-default disabled:opacity-45";
    let btn_round_on = "inline-flex h-7 w-7 items-center justify-center rounded-full border-0 bg-transparent text-emerald-400 opacity-100 hover:bg-emerald-400/20 disabled:cursor-default disabled:opacity-45";

    view! {
        <header class="flex w-full items-center justify-between gap-2 pt-0.5 px-0.5">
            <span
                class="pl-1 text-xs tabular-nums text-zinc-500"
                data-testid="app-version"
                title="fpmedia-web-components"
            >
                {VERSION}
            </span>
            <div class="flex items-center gap-2">
                <button
                    type="button"
                    class=move || {
                        if debug_on.get() {
                            btn_round_on.to_string()
                        } else {
                            btn_round.to_string()
                        }
                    }
                    data-testid="btn-debug"
                    prop:aria-pressed=move || debug_on.get()
                    prop:aria-label=move || {
                        if debug_on.get() {
                            "Disable debug info"
                        } else {
                            "Enable debug info"
                        }
                    }
                    prop:title=move || {
                        if debug_on.get() {
                            "Disable debug info"
                        } else {
                            "Enable debug info"
                        }
                    }
                    on:click=move |_| debug_on.update(|v| *v = !*v)
                >
                    <icon::DebugInfo class="h-5 w-5 shrink-0"/>
                </button>
                <a
                    class=btn_round
                    href="https://github.com/alexantoshuk/fpmedia-web"
                    target="_blank"
                    rel="noopener noreferrer"
                    data-testid="btn-github"
                    aria-label="fpmedia-web on GitHub"
                    title="fpmedia-web on GitHub"
                >
                    <icon::GitHub class="h-5 w-5 shrink-0"/>
                </a>
            </div>
        </header>
        <div class="mx-auto w-full max-w-[1100px] px-0 pb-6">
            <h1 class="mb-4 text-center text-xl font-semibold tracking-tight text-emerald-400">
                "Frame-precise media player"
            </h1>

            <div class="mb-3 flex flex-wrap items-center justify-center gap-2" data-testid="source">
                <label class=format!("wm-file {btn}")>
                    "Select local file(s)"
                    <input
                        type="file"
                        accept="video/mp4,video/*"
                        multiple
                        data-testid="file-input"
                        on:change=move |ev: leptos::ev::Event| {
                            let Some(input) = event_target::<HtmlInputElement>(&ev) else {
                                return;
                            };
                            let Some(files) = input.files() else { return };
                            open_demo_files(video, playlist, session, files, clip_label);
                        }
                    />
                </label>
                <button
                    class=btn
                    data-testid="btn-load-url"
                    on:click=move |_| {
                        let Some(w) = window() else { return };
                        let Ok(Some(raw)) = w.prompt_with_message_and_default(
                            "Enter media URL(s). Separate several with `;`. https or same-origin /media/…; CORS / Range required.",
                            "https://lorem.video/720p",
                        ) else {
                            return;
                        };
                        if let Some(input) = url_input.get_untracked() {
                            input.set_value(&raw);
                        }
                        open_demo_urls(video, playlist, session, raw, clip_label);
                    }
                >
                    "Load remote URL"
                </button>
            </div>

            <p
                class="mb-4 min-h-5 truncate text-center font-mono text-sm text-zinc-400"
                data-testid="clip-label"
                title=move || clip_label.get().unwrap_or_default()
            >
                {move || clip_label.get().unwrap_or_default()}
            </p>

            // e2e hooks — off-screen (not display:none) so Playwright fill/click works.
            <div class="fixed top-0 -left-[9999px]" aria-hidden="true">
                <input
                    type="url"
                    node_ref=url_input
                    data-testid="url-input"
                />
                <button
                    type="button"
                    data-testid="btn-open-url"
                    on:click=move |_| {
                        let Some(input) = url_input.get() else { return };
                        open_demo_urls(video, playlist, session, input.value(), clip_label);
                    }
                >
                    "Open URL"
                </button>
            </div>

            <div
                class=move || {
                    // `@container` — chrome responds to player width (embed-safe), not viewport.
                    if fullscreen.get() && !controls_visible.get() {
                        "wm-player @container/player is-controls-hidden overflow-hidden bg-zinc-900"
                    } else {
                        "wm-player @container/player overflow-hidden bg-zinc-900"
                    }
                }
                data-testid="player"
                node_ref=player_root
            >
                <div
                    class="wm-media flex flex-col"
                    data-testid="media-stage"
                    on:click=move |_| {
                        if !media_ready.get_untracked() {
                            return;
                        }
                        let media = demo_media_untracked(video, playlist, session);
                        if media_playing.get_untracked() {
                            media.pause();
                        } else {
                            media.play();
                        }
                    }
                >
                    // One host at a time — open.rs closes the other then remounts before open.
                    <Show
                        when=move || session.get().is_playlist()
                        fallback=move || view! { <Video video=video/> }
                    >
                        <VideoPlaylist playlist=playlist/>
                    </Show>
                </div>
                <div
                    class="wm-player-ui flex flex-col"
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
                    <Show when=move || scrubber_on.get()>
                        <Show
                            when=move || session.get().is_playlist()
                            fallback=move || {
                                view! {
                                    <DemoScrubber
                                        video=video
                                        thumbs_on=thumbs_on
                                        no_hover=no_hover
                                        position_format=position_format
                                    />
                                }
                            }
                        >
                            <DemoPlaylistScrubber
                                playlist=playlist
                                thumbs_on=thumbs_on
                                no_hover=no_hover
                                position_format=position_format
                            />
                        </Show>
                    </Show>
                    <div
                        class="wm-controls relative flex w-full flex-nowrap items-center gap-1 px-1 py-3"
                        data-testid="controls"
                    >
                        <div class="wm-controls-side flex shrink-0 flex-nowrap items-center justify-start gap-1">
                        <button
                            type="button"
                            class=move || {
                                if media_playing.get() {
                                    format!("{btn_play} is-playing")
                                } else {
                                    btn_play.to_string()
                                }
                            }
                            data-testid="btn-play-pause"
                            prop:disabled=move || !media_ready.get()
                            prop:aria-label=move || {
                                if media_playing.get() { "Pause" } else { "Play" }
                            }
                            prop:title=move || {
                                if media_playing.get() { "Pause" } else { "Play" }
                            }
                            on:click=move |_| {
                                demo_toggle_play(
                                    demo_media_untracked(video, playlist, session),
                                    media_playing.get_untracked(),
                                );
                            }
                        >
                            <icon::Play class="wm-icon wm-icon-play"/>
                            <icon::Pause class="wm-icon wm-icon-pause"/>
                        </button>
                        <button
                            type="button"
                            class=move || {
                                if media_looping.get() {
                                    btn_icon_on.to_string()
                                } else {
                                    btn_icon.to_string()
                                }
                            }
                            data-testid="btn-loop"
                            prop:disabled=move || !media_ready.get()
                            prop:aria-pressed=move || media_looping.get()
                            prop:aria-label=move || {
                                if media_looping.get() { "Loop on" } else { "Loop off" }
                            }
                            prop:title=move || {
                                if media_looping.get() { "Loop on" } else { "Loop off" }
                            }
                            on:click=move |_| {
                                demo_toggle_loop(demo_media_untracked(video, playlist, session));
                            }
                        >
                            <icon::Loop class="wm-icon wm-icon-loop"/>
                        </button>
                        <button
                            type="button"
                            class=btn_icon_step
                            data-testid="btn-prev"
                            prop:disabled=move || !media_ready.get()
                            prop:aria-label="Previous frame"
                            prop:title="Previous frame"
                            on:click=move |_| {
                                demo_previous_frame(demo_media_untracked(video, playlist, session));
                            }
                        >
                            <icon::NextFrame class="wm-icon wm-icon-prev -scale-x-100"/>
                        </button>
                        <button
                            type="button"
                            class=btn_icon_step
                            data-testid="btn-next"
                            prop:disabled=move || !media_ready.get()
                            prop:aria-label="Next frame"
                            prop:title="Next frame"
                            on:click=move |_| {
                                demo_next_frame(demo_media_untracked(video, playlist, session));
                            }
                        >
                            <icon::NextFrame class="wm-icon wm-icon-next"/>
                        </button>
                        </div>
                        <div class="flex min-w-0 flex-1 items-center justify-center">
                            <PositionDisplay
                                video=video
                                playlist=playlist
                                session=session
                                open=position_open
                                format=position_format
                                menu=position_menu
                                trigger=position_trigger
                                rate_open=rate_open
                                volume_open=volume_open
                                settings_open=settings_open
                            />
                        </div>
                        <div class="wm-controls-side flex shrink-0 flex-nowrap items-center justify-end gap-1">
                        <div
                            class=move || {
                                let mut c = String::from(
                                    "wm-volume-control group relative flex shrink-0 items-center",
                                );
                                if volume_open.get() {
                                    c.push_str(" is-open");
                                }
                                if volume_sliding.get() {
                                    c.push_str(" is-sliding");
                                }
                                c
                            }
                            node_ref=volume_root
                            data-testid="volume"
                            on:mouseleave=move |_| {
                                // Hover devices: drop tap-latch so V can fade with :hover.
                                // Coarse: `is-open` stays until outside click / second tap.
                                if !no_hover.get_untracked() {
                                    volume_open.set(false);
                                }
                            }
                        >
                            <button
                                type="button"
                                // Mute swaps Volume0 — not an "on" emerald latch (Loop / fullscreen).
                                class=btn_icon
                                data-testid="btn-mute"
                                prop:disabled=move || {
                                    let v = demo_media(video, playlist, session).active_video();
                                    !media_ready.get() || !v.has_audio.get()
                                }
                                prop:aria-pressed=move || {
                                    demo_media(video, playlist, session).muted.get()
                                }
                                prop:aria-label=move || {
                                    if demo_media(video, playlist, session).muted.get() {
                                        "Unmute"
                                    } else {
                                        "Mute"
                                    }
                                }
                                prop:title=move || {
                                    if demo_media(video, playlist, session).muted.get() {
                                        "Unmute"
                                    } else {
                                        "Mute"
                                    }
                                }
                                on:click=move |_| {
                                    // Coarse / no-hover: first tap opens slider; later taps mute.
                                    if no_hover.get_untracked() && !volume_open.get_untracked() {
                                        volume_open.set(true);
                                        rate_open.set(false);
                                        settings_open.set(false);
                                        return;
                                    }
                                    let media =
                                        demo_media_untracked(video, playlist, session);
                                    let vol = media.volume.get_untracked();
                                    // Volume already 0: mute toggle is a no-op visually — unmute + max.
                                    if vol <= 0.0 {
                                        media.set_volume(1.0);
                                        media.set_muted(false);
                                        return;
                                    }
                                    media.set_muted(!media.muted.get_untracked());
                                }
                            >
                                {move || {
                                    let muted = demo_media(video, playlist, session).muted.get();
                                    let vol = demo_media(video, playlist, session).volume.get();
                                    if muted || vol <= 0.0 {
                                        EitherOf3::A(view! { <icon::Volume0 class="wm-icon"/> })
                                    } else if vol < 0.5 {
                                        EitherOf3::B(view! { <icon::Volume1 class="wm-icon"/> })
                                    } else {
                                        EitherOf3::C(view! { <icon::Volume2 class="wm-icon"/> })
                                    }
                                }}
                            </button>
                            <div
                                class="wm-volume-slider wm-volume-slider-v"
                                role="dialog"
                                aria-label="Volume"
                            >
                                <VolumeRangeInput
                                    video=video
                                    playlist=playlist
                                    session=session
                                    volume_sliding=volume_sliding
                                    no_hover=no_hover
                                />
                            </div>
                        </div>
                        <div
                            class=move || {
                                // Same tier as step buttons (`@3xl` ~768px); below → settings overflow.
                                let open = if rate_open.get() { " is-open" } else { "" };
                                format!("wm-rate relative shrink-0 hidden @3xl:block{open}")
                            }
                            node_ref=rate_root
                            data-testid="playback-rate"
                        >
                            <button
                                type="button"
                                class=move || {
                                    let r = demo_media(video, playlist, session).playback_rate.get();
                                    if rate_is_unity(r) {
                                        btn_rate.to_string()
                                    } else {
                                        btn_rate_accent.to_string()
                                    }
                                }
                                prop:disabled=move || !media_ready.get()
                                prop:aria-expanded=move || rate_open.get()
                                prop:aria-label="Playback rate"
                                prop:title="Playback rate (0.25–1.75; double-click → 1×; JKL 1/2/4/8)"
                                on:click=move |_| {
                                    let next = !rate_open.get_untracked();
                                    rate_open.set(next);
                                    if next {
                                        volume_open.set(false);
                                        settings_open.set(false);
                                    }
                                }
                                on:dblclick=move |ev| {
                                    ev.prevent_default();
                                    demo_media_untracked(video, playlist, session).set_playback_rate(1.0);
                                    rate_open.set(false);
                                }
                            >
                                {move || {
                                    let r = demo_media(video, playlist, session).playback_rate.get();
                                    if rate_is_unity(r)
                                        && let Some(fps) =
                                            demo_source_fps(demo_media(video, playlist, session))
                                    {
                                        return format_fps_label(fps);
                                    }
                                    format!("{:.2}x", r)
                                }}
                            </button>
                            <div
                                class="wm-rate-pop"
                                role="dialog"
                                aria-label="Playback rate"
                            >
                                <PlaybackRateTrack
                                    video=video
                                    playlist=playlist
                                    session=session
                                    media_ready=media_ready
                                    input_testid="playback-rate-input"
                                />
                            </div>
                        </div>
                        <div
                            class=move || {
                                let open = if settings_open.get() { " is-open" } else { "" };
                                format!("wm-settings wm-bar-menu-host shrink-0{open}")
                            }
                            data-testid="settings"
                        >
                            <button
                                type="button"
                                node_ref=settings_trigger
                                class=move || {
                                    if settings_open.get() {
                                        btn_icon_on.to_string()
                                    } else {
                                        btn_icon.to_string()
                                    }
                                }
                                data-testid="btn-settings"
                                prop:disabled=move || !media_ready.get()
                                prop:aria-expanded=move || settings_open.get()
                                prop:aria-haspopup="menu"
                                prop:aria-label="More controls"
                                prop:title="More controls"
                                on:click=move |_| {
                                    let next = !settings_open.get_untracked();
                                    settings_open.set(next);
                                    if next {
                                        rate_open.set(false);
                                        volume_open.set(false);
                                        position_open.set(false);
                                    }
                                }
                            >
                                <icon::Settings class="wm-icon"/>
                            </button>
                            <div
                                class="wm-bar-menu wm-bar-menu--end wm-settings-menu"
                                node_ref=settings_menu
                                role="menu"
                                aria-label="More controls"
                                data-testid="settings-menu"
                            >
                                <button
                                    type="button"
                                    class="wm-settings-item"
                                    role="menuitem"
                                    data-testid="btn-loop-in"
                                    prop:disabled=move || !media_ready.get()
                                    on:click=move |_| {
                                        demo_mark_loop_in(demo_media_untracked(video, playlist, session));
                                        settings_open.set(false);
                                    }
                                >
                                    "Set Loop In"
                                    <span class="wm-settings-kbd">"["</span>
                                </button>
                                <button
                                    type="button"
                                    class="wm-settings-item"
                                    role="menuitem"
                                    data-testid="btn-loop-out"
                                    prop:disabled=move || !media_ready.get()
                                    on:click=move |_| {
                                        demo_mark_loop_out(demo_media_untracked(video, playlist, session));
                                        settings_open.set(false);
                                    }
                                >
                                    "Set Loop Out"
                                    <span class="wm-settings-kbd">"]"</span>
                                </button>
                                <button
                                    type="button"
                                    class="wm-settings-item"
                                    role="menuitem"
                                    data-testid="btn-loop-zone-reset"
                                    prop:disabled=move || !media_ready.get()
                                    on:click=move |_| {
                                        demo_reset_loop_zone(demo_media_untracked(video, playlist, session));
                                        settings_open.set(false);
                                    }
                                >
                                    "Reset Loop Zone"
                                </button>
                                <div
                                    class="wm-settings-overflow flex flex-col gap-0.5 @3xl:hidden"
                                    role="group"
                                    aria-label="Playback rate"
                                >
                                    <div class="wm-settings-menu-sep"></div>
                                    <div class="wm-settings-rate">
                                        <PlaybackRateTrack
                                            video=video
                                            playlist=playlist
                                            session=session
                                            media_ready=media_ready
                                            input_testid="playback-rate-input-menu"
                                        />
                                    </div>
                                </div>
                            </div>
                        </div>
                        <button
                            type="button"
                            class=move || {
                                // Icon swap (enter/exit) carries state — no accent.
                                let base = if fullscreen.get() {
                                    format!("{btn_icon} is-fullscreen")
                                } else {
                                    btn_icon.to_string()
                                };
                                format!("{base} shrink-0")
                            }
                            data-testid="btn-fullscreen"
                            prop:aria-pressed=move || fullscreen.get()
                            prop:aria-label=move || {
                                if fullscreen.get() { "Exit fullscreen" } else { "Fullscreen" }
                            }
                            prop:title=move || {
                                if fullscreen.get() { "Exit fullscreen" } else { "Fullscreen" }
                            }
                            on:click=move |_| {
                                let Some(root) = player_root.get_untracked() else { return };
                                let Ok(el) = root.dyn_into::<HtmlElement>() else { return };
                                let Some(doc) = window().and_then(|w| w.document()) else { return };
                                if fullscreen.get_untracked() {
                                    doc.exit_fullscreen();
                                } else {
                                    let _ = el.request_fullscreen();
                                }
                            }
                        >
                            <icon::FullScreenEnter class="wm-icon wm-icon-fullscreen"/>
                            <icon::FullScreenExit class="wm-icon wm-icon-fullscreen-exit"/>
                        </button>
                        </div>
                    </div>
                </div>
            </div>

            // Debug panel — when off: collapse without display:none so e2e
            // `innerText` on status/meta/pipeline still works.
            <div
                class=move || {
                    if debug_on.get() {
                        "px-2 mt-4 flex flex-col gap-2"
                    } else {
                        "pointer-events-none m-0 h-0 overflow-hidden p-0 opacity-0"
                    }
                }
                data-testid="debug-panel"
            >
                <div
                    class="flex flex-wrap items-center gap-4"
                    data-testid="debug-toggles"
                >
                    <label
                        class="inline-flex cursor-pointer items-center gap-1.5 text-sm text-zinc-300 select-none"
                        title="Timeline scrubber. Off = no scrub decoder / densify / strip caches."
                    >
                        <input
                            type="checkbox"
                            class="accent-emerald-400"
                            data-testid="chk-scrubber"
                            prop:checked=move || scrubber_on.get()
                            on:change=move |ev| {
                                let Some(input) = event_target::<HtmlInputElement>(&ev) else {
                                    return;
                                };
                                scrubber_on.set(input.checked());
                            }
                        />
                        "Scrub"
                    </label>
                    <label
                        class="inline-flex cursor-pointer items-center gap-1.5 text-sm text-zinc-300 select-none"
                        title="Hover thumbs (ThumbPolicy). Needs Scrub on. Off when (hover: none)."
                    >
                        <input
                            type="checkbox"
                            class="accent-emerald-400"
                            data-testid="chk-thumbs"
                            prop:checked=move || thumbs_on.get() && !no_hover.get()
                            prop:disabled=move || !scrubber_on.get() || no_hover.get()
                            on:change=move |ev| {
                                let Some(input) = event_target::<HtmlInputElement>(&ev) else {
                                    return;
                                };
                                thumbs_on.set(input.checked());
                            }
                        />
                        "Thumbnails"
                    </label>
                    <label
                        class="inline-flex items-center gap-1.5 text-sm text-zinc-300 select-none"
                        title="Thumb decode box (set_thumb_max_size). Clears + refills hover / Pause thumbs."
                    >
                        "Thumb size"
                        <select
                            class="rounded bg-zinc-800 px-1 text-zinc-200"
                            data-testid="sel-thumb-size"
                            prop:value=move || thumb_box.get().to_string()
                            prop:disabled=move || !scrubber_on.get() || !thumbs_on.get() || no_hover.get()
                            on:change=move |ev| {
                                if let Ok(n) = event_target_value(&ev).parse::<u32>() {
                                    thumb_box.set(n);
                                }
                            }
                        >
                            <option value="80">"80"</option>
                            <option value="160">"160"</option>
                            <option value="320">"320"</option>
                        </select>
                    </label>
                </div>
                <div class="wm-stats-head flex items-start gap-2">
                    <div class="flex min-w-0 flex-1 flex-col gap-0 text-sm leading-5 tabular-nums text-zinc-400">
                        <p class="wm-meta m-0 min-h-5" data-testid="meta">
                            {move || {
                                let v = demo_media(video, playlist, session).active_video();
                                let label = if session.get().is_playlist() {
                                    clip_label.get().map(|s| {
                                        format!(
                                            "playlist · clip {}/{} · {s}",
                                            playlist.live_clip.get() + 1,
                                            playlist.model.get().len().max(1)
                                        )
                                    })
                                } else {
                                    clip_label.get()
                                };
                                demo_meta_line(&v, label)
                            }}
                        </p>
                        <p class="wm-status m-0 min-h-5" data-testid="status">
                            {move || {
                                if session.get().is_playlist() && (playlist.busy.get() || !playlist.ready.get())
                                {
                                    return playlist.status.get();
                                }
                                demo_status_line(
                                    &demo_media(video, playlist, session).active_video(),
                                )
                            }}
                        </p>
                        <p
                            class="wm-pipeline m-0 min-h-10 whitespace-pre-line"
                            data-testid="pipeline"
                        >
                            {move || {
                                demo_pipeline_line(
                                    demo_media(video, playlist, session).active_video()
                                        .pipeline
                                        .get(),
                                )
                            }}
                        </p>
                    </div>
                    <button
                        type="button"
                        class=move || {
                            if stats_copied.get() {
                                btn_icon_on.to_string()
                            } else {
                                btn_icon.to_string()
                            }
                        }
                        data-testid="btn-copy-stats"
                        prop:aria-label=move || {
                            if stats_copied.get() { "Stats copied" } else { "Copy stats" }
                        }
                        prop:title=move || {
                            if stats_copied.get() {
                                "Copied"
                            } else {
                                "Copy meta · status · pipeline"
                            }
                        }
                        on:click={
                            let copy_stats = copy_stats.clone();
                            move |_| {
                                let v = demo_media(video, playlist, session).active_video();
                                let text = demo_stats_clipboard(&v, clip_label.get());
                                copy_stats(&text);
                            }
                        }
                    >
                        <icon::Copy class="wm-icon wm-icon-copy"/>
                    </button>
                </div>
            </div>

            // Machine-readable PipelineStats for e2e — always mounted, never shown.
            <pre
                class="wm-pipeline-json"
                data-testid="pipeline-json"
                hidden
                aria-hidden="true"
            >
                {move || {
                    pipeline_stats_json(
                        demo_media(video, playlist, session).active_video()
                            .pipeline
                            .get(),
                    )
                }}
            </pre>
            <p
                class="wm-error mt-3 min-h-5 whitespace-pre-wrap text-red-400"
                data-testid="error"
            >
                {move || {
                    if session.get().is_playlist() {
                        playlist
                            .error
                            .get()
                            .or_else(|| {
                                demo_media(video, playlist, session).active_video().error.get()
                            })
                            .unwrap_or_default()
                    } else {
                        video.error.get().unwrap_or_default()
                    }
                }}
            </p>
        </div>
    }
}
