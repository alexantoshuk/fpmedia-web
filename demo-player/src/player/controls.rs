use super::format::{
    RATE_SLIDER_MAX, RATE_SLIDER_MIN, RATE_TICKS, format_fps_label_precise, format_rate_tick,
    rate_tick_left_pct,
};
use super::media_ops::{DemoSession, demo_media, demo_media_untracked, demo_source_fps};
use fpmedia_web_components::{VideoPlaylistRef, VideoRef};
use leptos::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::JsFuture;
use web_sys::{HtmlElement, HtmlInputElement, OrientationLockType, OrientationType, window};

/// Android Chrome (etc.): after fullscreen, lock landscape if still portrait.
/// iOS / desktop: `lock()` rejects — swallow. Exit fullscreen unlocks per spec.
pub(super) fn try_lock_fs_landscape() {
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

pub(super) fn try_unlock_orientation() {
    let Some(w) = window() else {
        return;
    };
    let Ok(screen) = w.screen() else {
        return;
    };
    let _ = screen.orientation().unlock();
}

/// Fullscreen chrome auto-hide after idle (ms).
pub(super) const CONTROLS_IDLE_MS: f64 = 2200.0;

pub(super) fn event_target<T: JsCast>(ev: &leptos::ev::Event) -> Option<T> {
    ev.target()?.dyn_into::<T>().ok()
}

/// Shared rate heading + slider + ticks (bar pop + settings overflow).
///
/// Takes video/playlist/`session` so [`demo_media`] rebinds when mode flips
/// (a stale [`MediaRef`] prop would keep the previous backend signals).
#[component]
pub(super) fn PlaybackRateTrack(
    video: VideoRef,
    playlist: VideoPlaylistRef,
    session: RwSignal<DemoSession>,
    media_ready: Memo<bool>,
    #[prop(into)] input_testid: String,
) -> impl IntoView {
    view! {
        <div class="wm-rate-panel">
            <div class="wm-rate-heading">
                {move || {
                    let media = demo_media(video, playlist, session);
                    let r = media.playback_rate.get();
                    match demo_source_fps(media) {
                        Some(fps) => format!(
                            "Playback rate {}",
                            format_fps_label_precise(fps * r)
                        ),
                        None => "Playback rate".to_string(),
                    }
                }}
            </div>
            <div class="wm-rate-track">
                <div class="wm-slider-h">
                    <input
                        type="range"
                        class="wm-rate-input"
                        attr:data-testid=input_testid
                        min="0.25"
                        max="1.75"
                        step="0.05"
                        prop:value=move || {
                            let r = demo_media(video, playlist, session).playback_rate.get();
                            if r < 0.0 {
                                "0.25".to_string()
                            } else {
                                r.min(1.75).to_string()
                            }
                        }
                        prop:disabled=move || !media_ready.get()
                        on:input=move |ev| {
                            let Some(input) = event_target::<HtmlInputElement>(&ev) else {
                                return;
                            };
                            if let Ok(v) = input.value().parse::<f64>() {
                                demo_media_untracked(video, playlist, session)
                                    .set_playback_rate(v);
                            }
                        }
                    />
                </div>
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
                                            let r = demo_media(video, playlist, session)
                                                .playback_rate
                                                .get()
                                                .clamp(RATE_SLIDER_MIN, RATE_SLIDER_MAX);
                                            if (r - t).abs() < 0.026 {
                                                "wm-rate-tick is-active"
                                            } else {
                                                "wm-rate-tick"
                                            }
                                        }
                                        style:left=left
                                        prop:disabled=move || !media_ready.get()
                                        prop:title=format!("{}×", format_rate_tick(t))
                                        on:click=move |ev| {
                                            ev.stop_propagation();
                                            demo_media_untracked(video, playlist, session)
                                                .set_playback_rate(t);
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
    }
}

/// Vertical volume range popover input.
#[component]
pub(super) fn VolumeRangeInput(
    video: VideoRef,
    playlist: VideoPlaylistRef,
    session: RwSignal<DemoSession>,
    volume_sliding: RwSignal<bool>,
    no_hover: Signal<bool>,
) -> impl IntoView {
    view! {
        <div class="wm-slider-v">
            <input
                type="range"
                class="wm-volume-input"
                data-testid="volume-input"
                min="0"
                max="1"
                step="0.05"
                style:--wm-volume-pct=move || {
                    let media = demo_media(video, playlist, session);
                    let pct = if media.muted.get() {
                        0.0
                    } else {
                        media.volume.get().clamp(0.0, 1.0) * 100.0
                    };
                    format!("{pct}%")
                }
                prop:value=move || {
                    // Mute parks the thumb at 0 without clearing stored volume;
                    // unmute restores playlist/single `volume`.
                    let media = demo_media(video, playlist, session);
                    if media.muted.get() {
                        "0".to_string()
                    } else {
                        media.volume.get().to_string()
                    }
                }
                prop:disabled=move || {
                    let v = demo_media(video, playlist, session).active_video();
                    !v.ready.get() || !v.has_audio.get()
                }
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
                        let media = demo_media_untracked(video, playlist, session);
                        media.set_volume(snapped.clamp(0.0, 1.0));
                        if media.muted.get_untracked() {
                            media.set_muted(false);
                        }
                    }
                }
            />
        </div>
    }
}
