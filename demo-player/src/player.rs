//! Demo controls over [`fpmedia_web_components::Video`] — not production player UI.

use crate::icon;
use fpmedia_web_components::helpers::{
    FRAME_ZEBRA_MIN_SLOT_PX, format_metadata, format_timecode_at, format_timecode_at_loose,
    frame_zebra_step, last_frame_id, odd_frame_runs, parse_position, playhead_tip_css,
    playhead_width_css, strip_canvas_buffer_width,
};
use fpmedia_web_components::{
    HoverThumb, HoverThumbCtx, PipelinePlane, PipelineStats, ReadyFrameKind, ScrubCachePolicy,
    Scrubber, ScrubberCtx, ThumbPolicy, TimelineMeta, TimelineMetaKind, Video, VideoRef,
};
use leptos::either::{Either, EitherOf3};
use leptos::ev;
use leptos::html::{Canvas, Div, Input};
use leptos::prelude::*;
use leptos_use::{
    UseClipboardReturn, UseTimeoutFnReturn, on_click_outside, use_clipboard, use_event_listener,
    use_media_query, use_resize_observer, use_timeout_fn,
};
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::JsFuture;
use web_sys::{
    Blob, Element, HtmlElement, HtmlInputElement, OrientationLockType, OrientationType, window,
};

/// Android Chrome (etc.): after fullscreen, lock landscape if still portrait.
/// iOS / desktop: `lock()` rejects — swallow. Exit fullscreen unlocks per spec.
fn try_lock_fs_landscape() {
    let Some(w) = window() else {
        return;
    };
    let Ok(screen) = w.screen() else {
        return;
    };
    let orient = screen.orientation();
    if let Ok(t) = orient.type_()
        && matches!(
            t,
            OrientationType::LandscapePrimary | OrientationType::LandscapeSecondary
        )
    {
        return;
    }
    let Ok(p) = orient.lock(OrientationLockType::Landscape) else {
        return;
    };
    wasm_bindgen_futures::spawn_local(async move {
        let _ = JsFuture::from(p).await;
    });
}

fn try_unlock_orientation() {
    let Some(w) = window() else {
        return;
    };
    let Ok(screen) = w.screen() else {
        return;
    };
    let _ = screen.orientation().unlock();
}

/// Matches `.wm-scrubber-track` background (`#2a2a2a`). Zebra even/odd =
/// this ± `contrast` per channel (demo chrome).
const SCRUBBER_TRACK_BASE: u8 = 0x2a;

/// Default zebra amplitude (~odd `#1a1a1a` under previous hard-coded paint).
const FRAME_ZEBRA_CONTRAST: u8 = 16;

/// Must match `demo-player/Trunk.toml` `[[proxy]]` backend (testvideo Release tag).
const TESTVIDEO_DOWNLOAD_BASE: &str =
    "https://github.com/alexantoshuk/testvideo/releases/download/v0.3.0/";

fn gray_hex(v: u8) -> String {
    format!("#{v:02x}{v:02x}{v:02x}")
}

/// Even / odd zebra colors from track base ± `contrast` (saturating).
fn zebra_pair_hex(contrast: u8) -> (String, String) {
    (
        gray_hex(SCRUBBER_TRACK_BASE.saturating_add(contrast)),
        gray_hex(SCRUBBER_TRACK_BASE.saturating_sub(contrast)),
    )
}

/// Forward shuttle ladder for demo J/L keys (K → 1× + play/pause).
const JKL_STEPS: [f64; 4] = [1.0, 2.0, 4.0, 8.0];

/// Review-rate tick marks in the rate popover (0.25…1.75).
const RATE_TICKS: [f64; 7] = [0.25, 0.5, 0.75, 1.0, 1.25, 1.5, 1.75];
const RATE_SLIDER_MIN: f64 = 0.25;
const RATE_SLIDER_MAX: f64 = 1.75;

/// Fullscreen chrome auto-hide after idle (ms).
const CONTROLS_IDLE_MS: f64 = 2200.0;

fn format_rate_tick(r: f64) -> String {
    if (r - r.round()).abs() < 1e-9 {
        format!("{}", r.round() as i32)
    } else {
        format!("{r}")
    }
}

fn rate_tick_left_pct(r: f64) -> String {
    let span = RATE_SLIDER_MAX - RATE_SLIDER_MIN;
    let p = ((r - RATE_SLIDER_MIN) / span * 100.0).clamp(0.0, 100.0);
    format!("{p}%")
}

/// Map known testvideo Release URLs / root-relative `/media/…` to absolute http(s) URLs.
/// `VideoRef::open_url` requires `http:`, `https:`, or `blob:` (not bare paths).
fn resolve_demo_open_url(raw: &str) -> String {
    let trimmed = raw.trim();
    let path = if let Some(rest) = trimmed.strip_prefix(TESTVIDEO_DOWNLOAD_BASE) {
        format!("/media/{rest}")
    } else {
        trimmed.to_string()
    };
    if path.starts_with('/')
        && let Some(origin) = window().and_then(|w| w.location().origin().ok())
    {
        return format!("{origin}{path}");
    }
    path
}

fn open_demo_url(video: &VideoRef, raw: String, clip_label: RwSignal<Option<String>>) {
    let trimmed = raw.trim().to_string();
    let resolved = resolve_demo_open_url(&trimmed);
    // Mediabunny-style label: file name or the URL string the user opened.
    clip_label.set(if trimmed.is_empty() {
        None
    } else {
        Some(trimmed)
    });
    video.open_url(resolved);
}

fn demo_meta_line(video: &VideoRef, clip_name: Option<String>) -> String {
    let mut s = String::new();
    if let Some(name) = clip_name {
        s.push_str(&name);
    }
    if let Some(meta) = video.metadata.get() {
        let m = format_metadata(&meta);
        if !m.is_empty() {
            if !s.is_empty() {
                s.push_str(" · ");
            }
            s.push_str(&m);
        }
    }
    if video.has_audio.get() {
        if !s.is_empty() {
            s.push_str(" · ");
        }
        s.push_str("audio");
    }
    s
}

fn demo_status_line(video: &VideoRef) -> String {
    if video.busy.get() || !video.ready.get() {
        return video.status.get();
    }
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
            .map(|m| m.gop_profile.label())
            .unwrap_or_else(|| "—".into()),
        video.timestamp.get(),
        fps_part,
        if video.playing.get() {
            " · playing"
        } else {
            ""
        },
        if video.looping.get() { " · loop" } else { "" }
    )
}

fn demo_pipeline_line(stats: Option<PipelineStats>) -> String {
    let Some(s) = stats else {
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
        format!(" · holds {}/{:.0}ms", s.underrun_holds, s.underrun_hold_ms)
    } else {
        String::new()
    };
    let dense = if s.scrub_busy { " · dense" } else { "" };
    let waits = match s.play_waits {
        Some(w) => format!(
            "\nwait/s rd {:.0} (max {:.0}) · out {:.0} · cap {:.0} · raf>50 {} (max {:.0})",
            w.read_ms, w.read_max_ms, w.out_wait_ms, w.cap_wait_ms, w.raf_stalls, w.raf_max_ms
        ),
        None => String::new(),
    };
    format!(
        "q {}/{} · held {} · decodeQ {} · skipped {}{}{}{}{}{}",
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
        waits,
    )
}

/// Snapshot for mobile paste into chat (meta / status / pipeline / error).
fn demo_stats_clipboard(video: &VideoRef, clip_name: Option<String>) -> String {
    let mut lines = Vec::new();
    let meta = demo_meta_line(video, clip_name);
    if !meta.is_empty() {
        lines.push(meta);
    }
    lines.push(demo_status_line(video));
    let pipe = demo_pipeline_line(video.pipeline.get());
    if !pipe.is_empty() {
        lines.push(pipe);
    }
    if let Some(err) = video.error.get()
        && !err.is_empty()
    {
        lines.push(err);
    }
    lines.join("\n")
}

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
    // Debug pipeline line + `pipeline-json` e2e — request meter plane only while demo lives.
    let meters_lease = video.lease_pipeline_plane(PipelinePlane::Meters);
    on_cleanup(move || drop(meters_lease));
    let player_root = NodeRef::<Div>::new();
    let fullscreen = RwSignal::new(false);
    // Fullscreen auto-hide of the control surface (not Google Chrome).
    let controls_visible = RwSignal::new(true);
    let over_controls = RwSignal::new(false);
    // Demo A/B (debug panel): hide timeline chrome / drop thumb sink.
    let scrubber_on = RwSignal::new(true);
    let thumbs_on = RwSignal::new(true);
    // Touch-primary (phone/tablet): no hover UI → no ThumbSink / progressive.
    // Laptops with a touchscreen usually stay `(hover: hover)` via trackpad/mouse.
    let no_hover = use_media_query("(hover: none)");
    let debug_on = RwSignal::new(false);
    // Rate / volume / position popovers.
    let rate_open = RwSignal::new(false);
    let volume_open = RwSignal::new(false);
    let position_open = RwSignal::new(false);
    let position_format = RwSignal::new(PositionFormat::Frames);
    // Keep V volume pop open while dragging past the panel.
    let volume_sliding = RwSignal::new(false);
    let rate_root = NodeRef::<Div>::new();
    let volume_root = NodeRef::<Div>::new();
    let position_root = NodeRef::<Div>::new();
    // File name or URL string from last open (clip line under the toolbar).
    let clip_label = RwSignal::new(None::<String>);
    let url_input = NodeRef::<Input>::new();
    let UseClipboardReturn {
        copied: stats_copied,
        copy: copy_stats,
        ..
    } = use_clipboard();
    Effect::new(move |_| {
        video.set_scrub_cache_policy(if scrubber_on.get() {
            ScrubCachePolicy::On
        } else {
            ScrubCachePolicy::Off
        });
    });
    Effect::new(move |_| {
        let on = scrubber_on.get() && thumbs_on.get() && !no_hover.get();
        video.set_thumb_policy(if on {
            ThumbPolicy::On
        } else {
            ThumbPolicy::Off
        });
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
            "Escape" => {
                rate_open.set(false);
                volume_open.set(false);
                position_open.set(false);
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
    let _position_outside = on_click_outside(position_root, move |_| {
        // Menu only — Go to edit keeps `open=false` and must not be cancelled here.
        position_open.set(false);
    });

    // Text buttons: filled bg, no border. Icon buttons: icon-only (no chrome).
    let btn = "inline-flex items-center justify-center rounded-md border-0 bg-zinc-800 px-3 py-2 text-sm text-zinc-100 hover:bg-zinc-700 disabled:cursor-default disabled:opacity-45";
    let btn_icon = "wm-btn-icon inline-flex h-9 w-9 items-center justify-center rounded-md border-0 bg-transparent text-zinc-100 hover:bg-zinc-800/60 disabled:cursor-default disabled:opacity-45";
    let btn_icon_on = "wm-btn-icon inline-flex h-9 w-9 items-center justify-center rounded-md border-0 bg-transparent text-emerald-400 hover:bg-zinc-800/60 disabled:cursor-default disabled:opacity-45";
    let btn_play = "wm-btn-icon inline-flex h-9 w-9 items-center justify-center rounded-md border-0 bg-transparent text-zinc-100 hover:bg-zinc-800/60 disabled:cursor-default disabled:opacity-45";
    // Step buttons: no bare `inline-flex` — it fights `@max-*`/`hidden` and leaves a gap.
    // Hidden until player container ≥ `@3xl` (~768px); then no layout space when gone.
    let btn_icon_step = "wm-btn-icon hidden @3xl:inline-flex h-9 w-9 items-center justify-center rounded-md border-0 bg-transparent text-zinc-100 hover:bg-zinc-800/60 disabled:cursor-default disabled:opacity-45";
    let btn_rate = "wm-rate-btn inline-flex h-9 min-w-11 items-center justify-center rounded-md border-0 bg-transparent px-1.5 text-sm tabular-nums text-zinc-300 hover:text-zinc-100 disabled:cursor-default disabled:opacity-45";

    let btn_round = "inline-flex h-7 w-7 items-center justify-center rounded-full border-0 bg-transparent text-zinc-100 hover:bg-zinc-800/60 disabled:cursor-default disabled:opacity-45";
    let btn_round_on = "inline-flex h-7 w-7 items-center justify-center rounded-full border-0 bg-transparent text-emerald-400 hover:bg-emerald-400/20 disabled:cursor-default disabled:opacity-45";

    view! {
        <header class="flex w-full items-center justify-end gap-2 pt-0.5 pr-0.5">
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
        </header>
        <div class="mx-auto w-full max-w-[1100px] px-0 pb-6">
            <h1 class="mb-4 text-center text-xl font-semibold tracking-tight text-emerald-400">
                "Frame-precise media player"
            </h1>

            <div class="mb-3 flex flex-wrap items-center justify-center gap-2" data-testid="source">
                <label class=format!("wm-file {btn}")>
                    "Select local file"
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
                            clip_label.set(Some(name.clone()));
                            video.status.set(format!("Selected {name}…"));
                            // File is a Blob — progressive open, no whole-file array_buffer.
                            let blob: Blob = file.unchecked_into();
                            video.open_blob(blob);
                        }
                    />
                </label>
                <button
                    class=btn
                    data-testid="btn-load-url"
                    on:click=move |_| {
                        let Some(w) = window() else { return };
                        let Ok(Some(raw)) = w.prompt_with_message_and_default(
                            "Enter a media URL (https or same-origin /media/…). It must support CORS / Range.",
                            "https://lorem.video/720p",
                        ) else {
                            return;
                        };
                        if let Some(input) = url_input.get_untracked() {
                            input.set_value(&raw);
                        }
                        open_demo_url(&video, raw, clip_label);
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
                        open_demo_url(&video, input.value(), clip_label);
                    }
                >
                    "Open URL"
                </button>
            </div>

            <div
                class=move || {
                    // `@container` — chrome responds to player width (embed-safe), not viewport.
                    if fullscreen.get() && !controls_visible.get() {
                        "wm-player @container is-controls-hidden overflow-hidden bg-zinc-900"
                    } else {
                        "wm-player @container overflow-hidden bg-zinc-900"
                    }
                }
                data-testid="player"
                node_ref=player_root
            >
                <div
                    class="wm-media flex flex-col cursor-pointer"
                    data-testid="media-stage"
                    on:click=move |_| {
                        if !video.ready.get_untracked() {
                            return;
                        }
                        if video.playing.get_untracked() {
                            video.pause();
                        } else {
                            video.play();
                        }
                    }
                >
                    <Video video=video/>
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
                        <DemoScrubber
                            video=video
                            thumbs_on=thumbs_on
                            no_hover=no_hover
                            position_format=position_format
                        />
                    </Show>
                    <div
                        class="wm-controls relative flex w-full flex-nowrap items-center gap-1 px-1 py-3"
                        data-testid="controls"
                    >
                        <div class="wm-controls-side flex shrink-0 flex-nowrap items-center justify-start gap-1">
                        <button
                            type="button"
                            class=move || {
                                if video.playing.get() {
                                    format!("{btn_play} is-playing")
                                } else {
                                    btn_play.to_string()
                                }
                            }
                            data-testid="btn-play-pause"
                            prop:disabled=move || !video.ready.get()
                            prop:aria-label=move || {
                                if video.playing.get() { "Pause" } else { "Play" }
                            }
                            prop:title=move || {
                                if video.playing.get() { "Pause" } else { "Play" }
                            }
                            on:click=move |_| {
                                if video.playing.get_untracked() {
                                    video.pause();
                                } else {
                                    video.play();
                                }
                            }
                        >
                            <icon::Play class="wm-icon wm-icon-play"/>
                            <icon::Pause class="wm-icon wm-icon-pause"/>
                        </button>
                        <button
                            type="button"
                            class=move || {
                                if video.looping.get() {
                                    btn_icon_on.to_string()
                                } else {
                                    btn_icon.to_string()
                                }
                            }
                            data-testid="btn-loop"
                            prop:disabled=move || !video.ready.get()
                            prop:aria-pressed=move || video.looping.get()
                            prop:aria-label=move || {
                                if video.looping.get() { "Loop on" } else { "Loop off" }
                            }
                            prop:title=move || {
                                if video.looping.get() { "Loop on" } else { "Loop off" }
                            }
                            on:click=move |_| video.set_loop(!video.looping.get_untracked())
                        >
                            <icon::Loop class="wm-icon wm-icon-loop"/>
                        </button>
                        <button
                            type="button"
                            class=btn_icon_step
                            data-testid="btn-prev"
                            prop:disabled=move || !video.ready.get()
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
                            <icon::NextFrame class="wm-icon wm-icon-prev -scale-x-100"/>
                        </button>
                        <button
                            type="button"
                            class=btn_icon_step
                            data-testid="btn-next"
                            prop:disabled=move || !video.ready.get()
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
                            <icon::NextFrame class="wm-icon wm-icon-next"/>
                        </button>
                        </div>
                        <div class="flex min-w-0 flex-1 items-center justify-center">
                            <PositionDisplay
                                video=video
                                open=position_open
                                format=position_format
                                root=position_root
                                rate_open=rate_open
                                volume_open=volume_open
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
                                class=move || {
                                    if video.muted.get() {
                                        format!("{btn_icon_on} is-on")
                                    } else {
                                        btn_icon.to_string()
                                    }
                                }
                                data-testid="btn-mute"
                                prop:disabled=move || !video.ready.get() || !video.has_audio.get()
                                prop:aria-pressed=move || video.muted.get()
                                prop:aria-label=move || {
                                    if video.muted.get() { "Unmute" } else { "Mute" }
                                }
                                prop:title=move || {
                                    if video.muted.get() { "Unmute" } else { "Mute" }
                                }
                                on:click=move |_| {
                                    // Coarse / no-hover: first tap opens slider; later taps mute.
                                    if no_hover.get_untracked() && !volume_open.get_untracked() {
                                        volume_open.set(true);
                                        rate_open.set(false);
                                        return;
                                    }
                                    // Volume already 0: mute toggle is a no-op visually — unmute + max.
                                    if video.volume.get_untracked() <= 0.0 {
                                        video.set_volume(1.0);
                                        video.set_muted(false);
                                        return;
                                    }
                                    video.set_muted(!video.muted.get_untracked());
                                }
                            >
                                {move || {
                                    let muted = video.muted.get();
                                    let vol = video.volume.get();
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
                                    volume_sliding=volume_sliding
                                    no_hover=no_hover
                                />
                            </div>
                        </div>
                        <div
                            class=move || {
                                // Next tier after step buttons (`@3xl`): rate needs `@4xl` (~896px).
                                let open = if rate_open.get() { " is-open" } else { "" };
                                format!("wm-rate shrink-0 hidden @4xl:block{open}")
                            }
                            node_ref=rate_root
                            data-testid="playback-rate"
                        >
                            <button
                                type="button"
                                class=btn_rate
                                prop:disabled=move || !video.ready.get()
                                prop:aria-expanded=move || rate_open.get()
                                prop:aria-label="Playback rate"
                                prop:title="Playback rate (0.25–1.75; double-click → 1×; JKL 1/2/4/8)"
                                on:click=move |_| {
                                    let next = !rate_open.get_untracked();
                                    rate_open.set(next);
                                    if next {
                                        volume_open.set(false);
                                    }
                                }
                                on:dblclick=move |ev| {
                                    ev.prevent_default();
                                    video.set_playback_rate(1.0);
                                    rate_open.set(false);
                                }
                            >
                                {move || format!("{:.2}x", video.playback_rate.get())}
                            </button>
                            <div
                                class="wm-rate-pop"
                                role="dialog"
                                aria-label="Playback rate"
                            >
                                <div class="wm-rate-track">
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
                                            let Some(input) = event_target::<HtmlInputElement>(&ev)
                                            else {
                                                return;
                                            };
                                            if let Ok(v) = input.value().parse::<f64>() {
                                                video.set_playback_rate(v);
                                            }
                                        }
                                    />
                                    <div class="wm-rate-ticks" aria-hidden="true">
                                        <div class="wm-rate-ticks-rail">
                                            {RATE_TICKS
                                                .into_iter()
                                                .map(|t| {
                                                    let left = rate_tick_left_pct(t);
                                                    view! {
                                                        <button
                                                            type="button"
                                                            class=move || {
                                                                let r = video
                                                                    .playback_rate
                                                                    .get()
                                                                    .clamp(
                                                                        RATE_SLIDER_MIN,
                                                                        RATE_SLIDER_MAX,
                                                                    );
                                                                if (r - t).abs() < 0.026 {
                                                                    "wm-rate-tick is-active"
                                                                } else {
                                                                    "wm-rate-tick"
                                                                }
                                                            }
                                                            style:left=left
                                                            prop:disabled=move || !video.ready.get()
                                                            prop:title=format!(
                                                                "{}×",
                                                                format_rate_tick(t)
                                                            )
                                                            on:click=move |ev| {
                                                                ev.stop_propagation();
                                                                video.set_playback_rate(t);
                                                            }
                                                        >
                                                            <span class="wm-rate-tick-mark"></span>
                                                            <span class="wm-rate-tick-label">
                                                                {format_rate_tick(t)}
                                                            </span>
                                                        </button>
                                                    }
                                                })
                                                .collect_view()}
                                        </div>
                                    </div>
                                </div>
                            </div>
                        </div>
                        <button
                            type="button"
                            class=move || {
                                let base = if fullscreen.get() {
                                    format!("{btn_icon_on} is-fullscreen")
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
                </div>
                <div class="wm-stats-head flex items-start gap-2">
                    <div class="flex min-w-0 flex-1 flex-col gap-0 text-sm leading-5 tabular-nums text-zinc-400">
                        <p class="wm-meta m-0 min-h-5" data-testid="meta">
                            {move || demo_meta_line(&video, clip_label.get())}
                        </p>
                        <p class="wm-status m-0 min-h-5" data-testid="status">
                            {move || demo_status_line(&video)}
                        </p>
                        <p
                            class="wm-pipeline m-0 min-h-10 whitespace-pre-line"
                            data-testid="pipeline"
                        >
                            {move || demo_pipeline_line(video.pipeline.get())}
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
                                let text = demo_stats_clipboard(&video, clip_label.get());
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
                {move || pipeline_stats_json(video.pipeline.get())}
            </pre>
            <p
                class="wm-error mt-3 min-h-5 whitespace-pre-wrap text-red-400"
                data-testid="error"
            >
                {move || video.error.get().unwrap_or_default()}
            </p>
        </div>
    }
}

/// Frame zebra on the progress track (demo chrome). Continuous `w/n` float
/// `fillRect` slots (same [`frame_slot`] as tip); off when step &lt; [`FRAME_ZEBRA_MIN_SLOT_PX`].
///
/// Measure a host **div** (not the canvas — `set_width` lies to RO/CSS). Below the
/// gate the canvas is unmounted (`Show`) so a wide bitmap cannot mush into subpixels.
#[component]
fn FrameZebra(contrast: u8) -> impl IntoView {
    let ctx = use_context::<ScrubberCtx>().expect("FrameZebra must be used inside <Scrubber>");
    let video = ctx.video;
    let host_ref = NodeRef::<Div>::new();
    let canvas_ref = NodeRef::<Canvas>::new();
    let track_w = RwSignal::new(0.0_f64);

    use_resize_observer(host_ref, move |entries, _| {
        let Some(entry) = entries.first() else {
            return;
        };
        let w = entry.content_rect().width();
        if w > 0.0 {
            track_w.set(w);
        }
    });

    let visible = Memo::new(move |_| {
        frame_zebra_step(video.frame_count.get(), track_w.get()) >= FRAME_ZEBRA_MIN_SLOT_PX
    });

    Effect::new(move |_| {
        if !visible.get() {
            return;
        }
        let count = video.frame_count.get();
        let css_w = track_w.get().max(0.0);
        let buf_w = strip_canvas_buffer_width(css_w, 1.0);
        let width = f64::from(buf_w);
        let Some(canvas) = canvas_ref.get() else {
            return;
        };
        let runs = odd_frame_runs(count, width);
        let (even, odd) = zebra_pair_hex(contrast);
        let _ = paint_frame_zebra(&canvas, buf_w, &runs, &even, &odd);
    });

    view! {
        <div class="wm-scrubber-zebra-host" node_ref=host_ref aria-hidden="true">
            <Show when=move || visible.get()>
                <canvas class="wm-scrubber-zebra" node_ref=canvas_ref aria-hidden="true" />
            </Show>
        </div>
    }
}

/// 1 px-tall zebra canvas: clear; if `odd_runs` empty leave transparent; else even + odds.
fn paint_frame_zebra(
    canvas: &web_sys::HtmlCanvasElement,
    buffer_width: u16,
    odd_runs: &[(f64, f64)],
    even: &str,
    odd: &str,
) -> bool {
    let w = u32::from(buffer_width.max(1));
    if canvas.width() != w || canvas.height() != 1 {
        canvas.set_width(w);
        canvas.set_height(1);
    }
    let Ok(Some(ctx_obj)) = canvas.get_context("2d") else {
        return false;
    };
    let Ok(ctx) = ctx_obj.dyn_into::<web_sys::CanvasRenderingContext2d>() else {
        return false;
    };
    let wf = f64::from(w);
    ctx.clear_rect(0.0, 0.0, wf, 1.0);
    if odd_runs.is_empty() {
        return true;
    }
    ctx.set_fill_style_str(even);
    ctx.fill_rect(0.0, 0.0, wf, 1.0);
    ctx.set_fill_style_str(odd);
    for &(lo, hi) in odd_runs {
        let span = hi - lo;
        if span > 0.0 {
            ctx.fill_rect(lo, 0.0, span, 1.0);
        }
    }
    true
}

/// Opaque tip — continuous `left` + `max(2px, 100%/n)` width (track clips EOF).
#[component]
fn ScrubPlayheadTip() -> impl IntoView {
    let ctx =
        use_context::<ScrubberCtx>().expect("ScrubPlayheadTip must be used inside <Scrubber>");
    let video = ctx.video;
    let tip = Memo::new(move |_| playhead_tip_css(video.playhead.get(), video.frame_count.get()));
    view! {
        <div
            class="wm-scrubber-playhead"
            data-testid="scrubber-playhead"
            aria-hidden="true"
            style:left=move || tip.get().0
            style:width=move || tip.get().1
        />
    }
}

/// Progress fill to tip left — continuous `calc(100% * f / n)`.
#[component]
fn ScrubProgress() -> impl IntoView {
    let ctx = use_context::<ScrubberCtx>().expect("ScrubProgress must be used inside <Scrubber>");
    let video = ctx.video;
    let width =
        Memo::new(move |_| playhead_width_css(video.playhead.get(), video.frame_count.get()));
    view! {
        <div
            class="wm-scrubber-progress"
            data-testid="scrubber-progress"
            style:width=move || width.get()
        />
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum PositionFormat {
    Frames,
    Timecode,
}

fn format_position_side(video: &VideoRef, frame: u32, format: PositionFormat) -> String {
    match format {
        PositionFormat::Frames => frame.to_string(),
        PositionFormat::Timecode => match video.metadata.get() {
            Some(m) => format_timecode_at_loose(&m, frame),
            None => frame.to_string(),
        },
    }
}

fn position_sides(video: &VideoRef, format: PositionFormat) -> (String, String) {
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

/// Frame.io-like center position: `current / last`, format menu, Go to… edit.
#[component]
fn PositionDisplay(
    video: VideoRef,
    open: RwSignal<bool>,
    format: RwSignal<PositionFormat>,
    root: NodeRef<Div>,
    rate_open: RwSignal<bool>,
    volume_open: RwSignal<bool>,
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
        if !video.ready.get_untracked() {
            return;
        }
        let Some(meta) = video.metadata.get_untracked() else {
            if let Ok(v) = raw.trim().parse::<u32>() {
                video.seek_frame(v);
            }
            return;
        };
        if let Some(frame) = parse_position(&meta, &raw) {
            video.seek_frame(frame);
        }
    };

    let cancel_goto = move || {
        editing.set(false);
        open.set(false);
    };

    let start_goto = move || {
        if !video.ready.get_untracked() {
            return;
        }
        let fmt = format.get_untracked();
        let n = video.frame_count.get_untracked();
        let cur = video.playhead.get_untracked().min(last_frame_id(n));
        draft.set(format_position_side(&video, cur, fmt));
        rate_open.set(false);
        volume_open.set(false);
        // Edit first, then close menu — never clear editing on menu close.
        editing.set(true);
        open.set(false);
    };

    view! {
        <div
            class=move || {
                if open.get() {
                    "wm-position shrink-0 is-open".to_string()
                } else {
                    "wm-position shrink-0".to_string()
                }
            }
            node_ref=root
            data-testid="position"
        >
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
                            data-testid="position-display"
                            prop:disabled=move || !video.ready.get()
                            prop:aria-expanded=move || open.get()
                            prop:aria-haspopup="menu"
                            prop:aria-label=move || {
                                let (cur, last) = position_sides(&video, format.get());
                                format!("{cur} / {last}")
                            }
                            prop:title="Position (format / Go to…)"
                            on:click=move |_| {
                                if !video.ready.get_untracked() {
                                    return;
                                }
                                let next = !open.get_untracked();
                                open.set(next);
                                if next {
                                    rate_open.set(false);
                                    volume_open.set(false);
                                }
                            }
                        >
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
                        </button>
                    })
                }
            }}
            <div
                class="wm-position-menu"
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
                    "Go to…"
                </button>
            </div>
        </div>
    }
}

/// Hover aim under the thumb — same Frames/Timecode format as position menu.
#[component]
fn HoverThumbFrameLabel(video: VideoRef, format: RwSignal<PositionFormat>) -> impl IntoView {
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

/// Demo scrubber chrome over headless [`Scrubber`] / [`TimelineMeta`] / [`HoverThumb`].
#[component]
fn DemoScrubber(
    video: VideoRef,
    thumbs_on: RwSignal<bool>,
    no_hover: Signal<bool>,
    position_format: RwSignal<PositionFormat>,
) -> impl IntoView {
    view! {
        <Scrubber video=video class="wm-scrubber".to_string()>
            <Show when=move || thumbs_on.get() && !no_hover.get()>
                <HoverThumb class="fpvideo-thumb-preview".to_string()>
                    <HoverThumbFrameLabel video=video format=position_format />
                </HoverThumb>
            </Show>
            <div class="wm-scrubber-track">
                <FrameZebra contrast=FRAME_ZEBRA_CONTRAST />
                <TimelineMeta
                    kind=TimelineMetaKind::Fetch
                    class="wm-scrubber-fetched".to_string()
                    test_id="scrubber-fetched"
                />
                <ScrubProgress />
                <ScrubPlayheadTip />
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

/// Vertical volume range popover input.
#[component]
fn VolumeRangeInput(
    video: VideoRef,
    volume_sliding: RwSignal<bool>,
    no_hover: Signal<bool>,
) -> impl IntoView {
    view! {
        <input
            type="range"
            class="wm-volume-input"
            data-testid="volume-input"
            min="0"
            max="1"
            step="0.05"
            prop:value=move || {
                // Mute parks the thumb at 0 without clearing stored volume;
                // unmute restores `video.volume`.
                if video.muted.get() {
                    "0".to_string()
                } else {
                    video.volume.get().to_string()
                }
            }
            prop:disabled=move || !video.ready.get() || !video.has_audio.get()
            on:pointerdown=move |ev| {
                // Drag keep-open = `is-sliding` only. Do not latch `volume_open`
                // (that stuck the V pop until click-outside).
                volume_sliding.set(true);
                if let Some(t) = ev.target()
                    && let Ok(el) = t.dyn_into::<HtmlElement>()
                {
                    let _ = el.set_pointer_capture(ev.pointer_id());
                }
            }
            on:pointerup=move |ev| {
                volume_sliding.set(false);
                // Hover path: drop focus so `:focus-within` does not pin the pop.
                if !no_hover.get_untracked()
                    && let Some(t) = ev.target()
                    && let Ok(el) = t.dyn_into::<HtmlElement>()
                {
                    let _ = el.blur();
                }
            }
            on:pointercancel=move |ev| {
                volume_sliding.set(false);
                if !no_hover.get_untracked()
                    && let Some(t) = ev.target()
                    && let Ok(el) = t.dyn_into::<HtmlElement>()
                {
                    let _ = el.blur();
                }
            }
            on:input=move |ev| {
                let Some(input) = event_target::<HtmlInputElement>(&ev) else {
                    return;
                };
                if let Ok(v) = input.value().parse::<f64>() {
                    // Snap to 0.05 so icon thresholds stay clean.
                    let snapped = (v / 0.05).round() * 0.05;
                    video.set_volume(snapped.clamp(0.0, 1.0));
                    if video.muted.get_untracked() {
                        video.set_muted(false);
                    }
                }
            }
        />
    }
}
