//! Hover timeline thumb — paint inside; host chrome via `children` / `class`.
//!
//! Spec: `.ai/product.md` § Element-layer helpers. Auto-wires into parent
//! [`crate::Scrubber`] when present. Manual [`crate::HoverThumbApi`] escape hatch OK.
//!
//! Decode max box = host API [`TimelineRef::thumb_max_size`] (same as Pause
//! progressive). Optional `width` / `height` props seed that API once.
//! Feature `thumbs` (default on): real paint. Soft stubs when off.
//! Play+pause hover; scrubber hides while scrubbing. Paint coalesce: per-instance
//! `leptos-use` trailing throttle. `mediabunny:` separate thumb `CanvasSink`.

use std::rc::Rc;

use leptos::html::{Canvas, Div};
use leptos::prelude::*;
use web_sys::HtmlCanvasElement;

use crate::TimelineRef;
use crate::VideoRef;
use crate::helpers::{hover_thumb_left, hover_thumb_transform_css};
use crate::scrubber::{HoverThumbApi, ScrubberCtx};

/// Default max-edge box (matches engine `DEFAULT_THUMB_MAX_*`).
pub const DEFAULT_HOVER_THUMB_MAX_W: u32 = fpmedia_web::DEFAULT_THUMB_MAX_W;
pub const DEFAULT_HOVER_THUMB_MAX_H: u32 = fpmedia_web::DEFAULT_THUMB_MAX_H;

/// Coalesce window before deferred Exact `paint_hover_thumb` (ms). This is the
/// dwell delay — engine has no second global aim floor (that left Exact stuck).
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
/// custom track. Optional [`timeline`] / [`video`] when not under a scrubber.
/// Optional `width` / `height` seed [`TimelineRef::set_thumb_max_size`] once;
/// canvas attrs follow the timeline signals (host CSS may stretch for display).
#[component]
pub fn HoverThumb(
    #[prop(optional)] video: Option<VideoRef>,
    #[prop(optional)] timeline: Option<TimelineRef>,
    /// Escape hatch / legacy wire-up — also filled when nested under [`crate::Scrubber`].
    #[prop(optional)]
    api: Option<StoredValue<Option<HoverThumbApi>, LocalStorage>>,
    /// Seed host thumb max width (decode box). Omit → keep timeline API value.
    #[prop(optional)]
    width: Option<u32>,
    /// Seed host thumb max height (decode box). Omit → keep timeline API value.
    #[prop(optional)]
    height: Option<u32>,
    #[prop(optional)] class: Option<String>,
    #[prop(optional)] children: Option<Children>,
) -> impl IntoView {
    let scrub_ctx = use_context::<ScrubberCtx>();
    let timeline = timeline
        .or_else(|| video.map(VideoRef::as_timeline))
        .or_else(|| scrub_ctx.map(|c| c.timeline))
        .expect("HoverThumb needs timeline=…, video=…, or a parent <Scrubber>");

    // Optional props seed the host API once (siblings share one box).
    if width.is_some() || height.is_some() {
        let (cur_w, cur_h) = timeline.thumb_max_size();
        timeline.set_thumb_max_size(width.unwrap_or(cur_w), height.unwrap_or(cur_h));
    }

    let max_w_sig = timeline.thumb_max_w_signal();
    let max_h_sig = timeline.thumb_max_h_signal();

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
                if timeline.ready.get_untracked() {
                    timeline.paint_hover_thumb(frame, dest, w, h);
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
        timeline.cancel_hover_thumb();
    });

    let update: Rc<dyn Fn(f64, f64, u32)> = {
        let hide = Rc::clone(&hide);
        #[cfg(feature = "thumbs")]
        let throttled_paint_move = throttled_paint.clone();
        Rc::new(move |x: f64, track_width: f64, aim: u32| {
            #[cfg(not(feature = "thumbs"))]
            {
                let _ = (x, track_width, aim, &timeline, &hide, root_ref);
                return;
            }
            #[cfg(feature = "thumbs")]
            {
                if !timeline.ready.get_untracked() {
                    hide();
                    return;
                }
                let (width, height) = (
                    max_w_sig.get_untracked().max(1),
                    max_h_sig.get_untracked().max(1),
                );
                // Clamp by laid-out root (canvas + host chrome). Decode box is
                // API max; CSS may stretch — use offsetWidth when visible.
                let thumb_w = root_ref
                    .get_untracked()
                    .map(|el| f64::from(el.offset_width()))
                    .filter(|w| *w > 1.0)
                    .unwrap_or(width as f64);
                let left = hover_thumb_left(x, track_width, thumb_w);
                transform.set(hover_thumb_transform_css(left));
                frame.set(Some(aim));
                if !visible.get_untracked() {
                    visible.set(true);
                }
                let Some(canvas) = canvas_ref.get_untracked() else {
                    return;
                };
                // Paint-first: cache/Exact nearest overwrites; cold miss keeps
                // prior pixels (incl. brief cross-clip stale) until Exact / idle
                // warm Effect — eager wipe left an empty tip. **no equivalent**.
                if timeline.try_paint_hover_thumb(aim, canvas.clone(), width, height) {
                    return;
                }
                let scheduled_epoch = epoch.get_value();
                throttled_paint_move((aim, canvas, width, height, scheduled_epoch));
            }
        })
    };

    Effect::new({
        let hide = Rc::clone(&hide);
        move |_| {
            if !timeline.ready.get() {
                hide();
            }
        }
    });

    // Playlist: after VT thumb wallet / producer fill, re-paint current aim
    // (pointer may be still — otherwise stale prior stays until move).
    #[cfg(feature = "thumbs")]
    Effect::new({
        let throttled_paint = throttled_paint.clone();
        move |_| {
            let Some(aim) = frame.get() else {
                return;
            };
            if !visible.get() || !timeline.ready.get() {
                return;
            }
            let Some(pl) = timeline.playlist() else {
                return;
            };
            let _ = pl.thumb_residency_epoch.get();
            let _ = pl.live_clip.get();
            let _ = pl.slot_a.ready.get();
            let _ = pl.slot_b.ready.get();
            let _ = max_w_sig.get();
            let _ = max_h_sig.get();
            let (width, height) = (
                max_w_sig.get_untracked().max(1),
                max_h_sig.get_untracked().max(1),
            );
            let Some(canvas) = canvas_ref.get_untracked() else {
                return;
            };
            if timeline.try_paint_hover_thumb(aim, canvas.clone(), width, height) {
                return;
            }
            let scheduled_epoch = epoch.get_value();
            throttled_paint((aim, canvas, width, height, scheduled_epoch));
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
        timeline.cancel_hover_thumb();
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
                width=move || max_w_sig.get().max(1).to_string()
                height=move || max_h_sig.get().max(1).to_string()
                aria-hidden="true"
            />
            {children.map(|c| c())}
        </div>
    }
}

/// Interim canvas-only thumb (class `fpvideo-thumb-preview`). Prefer [`HoverThumb`].
#[component]
pub fn HoverThumbPreview(
    video: VideoRef,
    api: StoredValue<Option<HoverThumbApi>, LocalStorage>,
    #[prop(optional)] max_w: Option<u32>,
    #[prop(optional)] max_h: Option<u32>,
) -> impl IntoView {
    // Seed host API (view props are optional u32 builders — not Option<_>).
    if max_w.is_some() || max_h.is_some() {
        let (cw, ch) = video.thumb_max_size();
        video.set_thumb_max_size(max_w.unwrap_or(cw), max_h.unwrap_or(ch));
    }
    view! {
        <HoverThumb
            video=video
            api=api
            class="fpvideo-thumb-preview".to_string()
        />
    }
}
