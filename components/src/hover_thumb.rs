//! Hover timeline thumb — paint inside; host chrome via `children` / `class`.
//!
//! Spec: `.ai/product.md` § Element-layer helpers. Auto-wires into parent
//! [`crate::Scrubber`] when present. Manual [`crate::HoverThumbApi`] escape hatch OK.
//!
//! Feature `thumbs` (default on): real paint. Soft stubs when off.
//! Play+pause hover; scrubber hides while scrubbing. Paint coalesce: per-instance
//! `leptos-use` trailing throttle. `mediabunny:` separate thumb `CanvasSink`.

use std::rc::Rc;

use leptos::html::{Canvas, Div};
use leptos::prelude::*;
use web_sys::HtmlCanvasElement;

use crate::VideoRef;
use crate::helpers::{hover_thumb_left, hover_thumb_transform_css};
use crate::scrubber::{HoverThumbApi, ScrubberCtx};

/// Default max-edge box (larger side ≤ 160; smaller from aspect via `hover_thumb_dims`).
pub const DEFAULT_HOVER_THUMB_MAX_W: u32 = 160;
pub const DEFAULT_HOVER_THUMB_MAX_H: u32 = 160;

/// Coalesce window before deferred `paint_hover_thumb` (ms). Engine aim-decode
/// floor is separate (`PLAY_THUMB_AIM_DECODE_MIN_MS` = 800).
const DEFER_HOVER_PAINT_MS: f64 = 150.0;

/// Signals for host chrome under [`HoverThumb`] (`use_context`).
#[derive(Clone, Copy)]
pub struct HoverThumbCtx {
    pub frame: RwSignal<Option<u32>>,
    pub visible: RwSignal<bool>,
}

/// Hover preview: canvas inside; `children` = chrome; position on root.
///
/// Prefer nesting under [`crate::Scrubber`] (auto-wire). Optional [`api`] for a
/// custom track. Optional [`video`] when not under a scrubber.
#[component]
pub fn HoverThumb(
    #[prop(optional)] video: Option<VideoRef>,
    /// Escape hatch / legacy wire-up — also filled when nested under [`crate::Scrubber`].
    #[prop(optional)]
    api: Option<StoredValue<Option<HoverThumbApi>, LocalStorage>>,
    #[prop(default = DEFAULT_HOVER_THUMB_MAX_W)] width: u32,
    #[prop(default = DEFAULT_HOVER_THUMB_MAX_H)] height: u32,
    #[prop(optional)] class: Option<String>,
    #[prop(optional)] children: Option<Children>,
) -> impl IntoView {
    let scrub_ctx = use_context::<ScrubberCtx>();
    let video = video
        .or_else(|| scrub_ctx.map(|c| c.video))
        .expect("HoverThumb needs video=… or a parent <Scrubber>");

    let frame = RwSignal::new(None::<u32>);
    let visible = RwSignal::new(false);
    provide_context(HoverThumbCtx { frame, visible });

    let root_ref = NodeRef::<Div>::new();
    let canvas_ref = NodeRef::<Canvas>::new();
    // Declarative `style:transform` — not `el.style().set_property` (skills/leptos.md).
    let transform = RwSignal::new(String::new());
    let epoch = StoredValue::new_local(0u64);
    let root_class = class.unwrap_or_default();

    #[cfg(feature = "thumbs")]
    let throttled_paint = {
        use leptos_use::{ThrottleOptions, use_throttle_fn_with_arg_and_options};
        use_throttle_fn_with_arg_and_options(
            move |(frame, dest, w, h, scheduled_epoch): (u32, HtmlCanvasElement, u32, u32, u64)| {
                if epoch.get_value() != scheduled_epoch {
                    return;
                }
                if video.ready.get_untracked() {
                    video.paint_hover_thumb(frame, dest, w, h);
                }
            },
            DEFER_HOVER_PAINT_MS,
            ThrottleOptions::default().leading(false).trailing(true),
        )
    };

    let hide: Rc<dyn Fn()> = Rc::new(move || {
        visible.set(false);
        frame.set(None);
        let e = epoch.get_value().wrapping_add(1);
        epoch.set_value(e);
        video.cancel_hover_thumb();
    });

    let update: Rc<dyn Fn(f64, f64, u32)> = {
        let hide = Rc::clone(&hide);
        Rc::new(move |x: f64, track_width: f64, aim: u32| {
            #[cfg(not(feature = "thumbs"))]
            {
                let _ = (x, track_width, aim, &video, &hide, width, height, root_ref);
                return;
            }
            #[cfg(feature = "thumbs")]
            {
                if !video.ready.get_untracked() {
                    hide();
                    return;
                }
                let left = hover_thumb_left(x, track_width, width as f64);
                transform.set(hover_thumb_transform_css(left));
                frame.set(Some(aim));
                if !visible.get_untracked() {
                    visible.set(true);
                }
                let Some(canvas) = canvas_ref.get_untracked() else {
                    return;
                };
                if video.try_paint_hover_thumb(aim, canvas.clone(), width, height) {
                    return;
                }
                let scheduled_epoch = epoch.get_value();
                throttled_paint((aim, canvas, width, height, scheduled_epoch));
            }
        })
    };

    Effect::new({
        let hide = Rc::clone(&hide);
        move |_| {
            if !video.ready.get() {
                hide();
            }
        }
    });

    let register = HoverThumbApi::new(Rc::clone(&hide), update);
    if let Some(api) = api {
        api.set_value(Some(register.clone()));
    }
    if let Some(ctx) = scrub_ctx {
        ctx.hover.set_value(Some(register));
    }

    on_cleanup(move || {
        let e = epoch.get_value().wrapping_add(1);
        epoch.set_value(e);
        visible.set(false);
        frame.set(None);
        video.cancel_hover_thumb();
        if let Some(api) = api {
            api.set_value(None);
        }
        if let Some(ctx) = scrub_ctx {
            ctx.hover.set_value(None);
        }
    });

    view! {
        <div
            node_ref=root_ref
            class=move || {
                if visible.get() {
                    format!("{root_class} is-visible")
                } else {
                    root_class.clone()
                }
            }
            style:transform=move || transform.get()
            data-testid="scrubber-thumb"
            aria-hidden="true"
        >
            <canvas
                node_ref=canvas_ref
                width=width.to_string()
                height=height.to_string()
                aria-hidden="true"
            />
            {children.map(|c| c())}
        </div>
    }
}

/// Interim canvas-only thumb (class `wm-thumb-preview`). Prefer [`HoverThumb`].
#[component]
pub fn HoverThumbPreview(
    video: VideoRef,
    api: StoredValue<Option<HoverThumbApi>, LocalStorage>,
    #[prop(default = DEFAULT_HOVER_THUMB_MAX_W)] max_w: u32,
    #[prop(default = DEFAULT_HOVER_THUMB_MAX_H)] max_h: u32,
) -> impl IntoView {
    view! {
        <HoverThumb
            video=video
            api=api
            width=max_w
            height=max_h
            class="wm-thumb-preview".to_string()
        />
    }
}
