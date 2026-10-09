//! Frame.io-style hover thumb chrome (not part of [`crate::Video`]).
//!
//! Hosts wire this next to a scrubber track and call [`HoverThumbApi`] from
//! pointer handlers. Engine paint stays on [`crate::VideoRef`]; this module owns
//! canvas, transform, coalesce, and cancel-on-leave.
//!
//! Feature `thumbs` (default on): real paint. Soft stubs when off.
//! Play+pause hover; host hides only while scrubbing (`mediabunny:` separate
//! thumb `CanvasSink`). Paint coalesce: per-instance `leptos-use` trailing
//! throttle (not process-global `thread_local!`).

use std::rc::Rc;

use leptos::html::Canvas;
use leptos::prelude::*;
use wasm_bindgen::JsCast;
use web_sys::{HtmlCanvasElement, HtmlElement};

use crate::VideoRef;
use crate::helpers::{hover_thumb_left, hover_thumb_transform_css};

/// Default CSS / canvas box (matches `.wm-thumb-preview` in `style/input.css`).
pub const DEFAULT_HOVER_THUMB_MAX_W: u32 = 160;
pub const DEFAULT_HOVER_THUMB_MAX_H: u32 = 90;

/// Coalesce window before deferred `paint_hover_thumb` (ms). Engine aim-decode
/// floor is separate (`PLAY_THUMB_AIM_DECODE_MIN_MS` = 800).
const DEFER_HOVER_PAINT_MS: f64 = 150.0;

/// Scrubber-facing handle — call from track `pointermove` / leave / scrub start.
#[derive(Clone)]
pub struct HoverThumbApi {
    hide: Rc<dyn Fn()>,
    /// `x` = pointer X relative to track left; `track_width`; presentation `frame`.
    update: Rc<dyn Fn(f64, f64, u32)>,
}

impl HoverThumbApi {
    pub fn hide(&self) {
        (self.hide)();
    }

    pub fn update(&self, x_in_track: f64, track_width: f64, frame: u32) {
        (self.update)(x_in_track, track_width, frame);
    }
}

/// Hover preview canvas — place as a sibling of the scrubber track (absolute).
///
/// Fill [`api`] once mounted; host calls [`HoverThumbApi::update`] /
/// [`HoverThumbApi::hide`] (scrub start / leave). Works during Play and Pause.
#[component]
pub fn HoverThumbPreview(
    video: VideoRef,
    /// Set to `Some` after mount; cleared on cleanup. Non-`Send` (`Rc` callbacks).
    api: StoredValue<Option<HoverThumbApi>, LocalStorage>,
    #[prop(default = DEFAULT_HOVER_THUMB_MAX_W)] max_w: u32,
    #[prop(default = DEFAULT_HOVER_THUMB_MAX_H)] max_h: u32,
) -> impl IntoView {
    let visible = RwSignal::new(false);
    let thumb_ref = NodeRef::<Canvas>::new();
    // Bumped on hide / cleanup so a trailing throttle cannot paint a stale aim.
    let epoch = StoredValue::new_local(0u64);

    #[cfg(feature = "thumbs")]
    let throttled_paint = {
        use leptos_use::{ThrottleOptions, use_throttle_fn_with_arg_and_options};
        use_throttle_fn_with_arg_and_options(
            move |(frame, dest, w, h, scheduled_epoch): (u32, HtmlCanvasElement, u32, u32, u64)| {
                if epoch.get_value() != scheduled_epoch {
                    return;
                }
                // Clip switch sets ready=false — do not paint into a retiring sink.
                if video.ready.get_untracked() {
                    video.paint_hover_thumb(frame, dest, w, h);
                }
            },
            DEFER_HOVER_PAINT_MS,
            // trailing: same 150 ms coalesce as the old TLS sleep; leading would
            // decode on every cache miss under the finger.
            ThrottleOptions::default().leading(false).trailing(true),
        )
    };

    let hide: Rc<dyn Fn()> = Rc::new(move || {
        visible.set(false);
        let e = epoch.get_value().wrapping_add(1);
        epoch.set_value(e);
        video.cancel_hover_thumb();
    });

    let update: Rc<dyn Fn(f64, f64, u32)> = {
        let hide = Rc::clone(&hide);
        Rc::new(move |x: f64, track_width: f64, frame: u32| {
            #[cfg(not(feature = "thumbs"))]
            {
                let _ = (x, track_width, frame, &video, &hide, max_w, max_h);
                return;
            }
            #[cfg(feature = "thumbs")]
            {
                if !video.ready.get_untracked() {
                    hide();
                    return;
                }
                let left = hover_thumb_left(x, track_width, max_w as f64);
                let Some(canvas) = thumb_ref.get_untracked() else {
                    return;
                };
                let html: &HtmlElement = canvas.unchecked_ref();
                let _ = html
                    .style()
                    .set_property("transform", &hover_thumb_transform_css(left));
                if !visible.get_untracked() {
                    visible.set(true);
                }
                if video.try_paint_hover_thumb(frame, canvas.clone(), max_w, max_h) {
                    return;
                }
                let scheduled_epoch = epoch.get_value();
                throttled_paint((frame, canvas, max_w, max_h, scheduled_epoch));
            }
        })
    };

    // Clip switch / close: `retire_clip` clears ready — hide without waiting for
    // pointerleave (stale preview canvas otherwise stays visible).
    Effect::new({
        let hide = Rc::clone(&hide);
        move |_| {
            if !video.ready.get() {
                hide();
            }
        }
    });

    api.set_value(Some(HoverThumbApi {
        hide: Rc::clone(&hide),
        update,
    }));

    on_cleanup(move || {
        let e = epoch.get_value().wrapping_add(1);
        epoch.set_value(e);
        visible.set(false);
        video.cancel_hover_thumb();
        api.set_value(None);
    });

    view! {
        <canvas
            node_ref=thumb_ref
            class=move || {
                if visible.get() {
                    "wm-thumb-preview is-visible"
                } else {
                    "wm-thumb-preview"
                }
            }
            data-testid="scrubber-thumb"
            width=max_w.to_string()
            height=max_h.to_string()
            aria-hidden="true"
        />
    }
}
