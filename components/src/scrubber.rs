//! Headless timeline scrubber — gesture + container + context.
//!
//! Hides transport / scrub phases from the host. Chrome (progress, ticks, layout)
//! is composed by the host via [`children`]. Spec: `.ai/product.md` § Element-layer
//! helpers; phases: `.ai/architecture.md` § Scrub phases.

use std::cell::Cell;
use std::rc::Rc;

use leptos::html::Div;
use leptos::prelude::*;
use leptos_use::{UseElementSizeReturn, use_element_size};
use wasm_bindgen::JsCast;
use web_sys::Element;

use crate::TimelineRef;
use crate::VideoRef;
use crate::helpers::{scrub_frame_from_x, scrub_touch_move_accepts};

/// Scrubber → hover wire — call from track `pointermove` / leave / scrub start.
#[derive(Clone)]
pub struct HoverThumbApi {
    pub(crate) hide: Rc<dyn Fn()>,
    /// `x` = pointer X relative to track left; `track_width`; presentation `frame`.
    pub(crate) update: Rc<dyn Fn(f64, f64, u32)>,
}

impl HoverThumbApi {
    pub fn hide(&self) {
        (self.hide)();
    }

    pub fn update(&self, x_in_track: f64, track_width: f64, frame: u32) {
        (self.update)(x_in_track, track_width, frame);
    }

    pub(crate) fn new(hide: Rc<dyn Fn()>, update: Rc<dyn Fn(f64, f64, u32)>) -> Self {
        Self { hide, update }
    }
}

/// Shared by nested [`crate::TimelineMeta`] / [`crate::HoverThumb`].
///
/// Scrub **domain** is always [`timeline`](TimelineRef). Pipeline strips follow
/// [`TimelineRef::strip_video`] (not a frozen `video=` prop) so playlist live
/// swaps never remount this scrubber.
#[derive(Clone, Copy)]
pub struct ScrubberCtx {
    /// Scrub / hover / strip domain (Video or Playlist).
    pub timeline: TimelineRef,
    /// Optional single-clip handle when opened via `video=` (BC / hosts that want it).
    /// Prefer [`TimelineRef::strip_video`] for pipeline paint.
    pub video: Option<VideoRef>,
    pub scrubbing: RwSignal<bool>,
    /// Track CSS width from [`use_element_size`].
    pub css_width: Signal<f64>,
    /// Filled by a nested hover thumb (auto-wire).
    pub(crate) hover: StoredValue<Option<HoverThumbApi>, LocalStorage>,
}

/// Track hit-target: pointer → playhead + `scrub_pointer_*`. Host supplies children.
///
/// Pass [`timeline`](TimelineRef) and/or [`video`](VideoRef). At least one is
/// required; `video=` fills `timeline` via [`VideoRef::as_timeline`]. Playlist
/// hosts should pass `timeline=playlist.as_timeline()` only — do not remount on
/// live swap.
#[component]
pub fn Scrubber(
    #[prop(optional)] video: Option<VideoRef>,
    #[prop(optional)] timeline: Option<TimelineRef>,
    #[prop(optional)] class: Option<String>,
    children: Children,
) -> impl IntoView {
    let timeline = timeline
        .or_else(|| video.map(VideoRef::as_timeline))
        .expect("Scrubber needs video=… or timeline=…");
    let video = video.or_else(|| timeline.video());

    let scrubbing = RwSignal::new(false);
    let hover = StoredValue::new_local(None::<HoverThumbApi>);
    // Touch soft-drag sticky `(last_x, last_frame)`; `None` = mouse/pen (1:1).
    let touch_sticky = Rc::new(Cell::new(None::<(f64, u32)>));
    let scrubber_ref = NodeRef::<Div>::new();
    let UseElementSizeReturn {
        width: css_width, ..
    } = use_element_size(scrubber_ref);

    provide_context(ScrubberCtx {
        timeline,
        video,
        scrubbing,
        css_width,
        hover,
    });

    let frame_from_client_x = move |el: &Element, client_x: f64| -> Option<u32> {
        if !timeline.ready.get_untracked() {
            return None;
        }
        let rect = el.get_bounding_client_rect();
        scrub_frame_from_x(
            client_x,
            rect.left(),
            rect.width(),
            timeline.frame_count.get_untracked(),
        )
    };

    let hide_hover = move || {
        hover.with_value(|api| {
            if let Some(api) = api.as_ref() {
                api.hide();
            }
        });
    };

    let finish_scrub = Rc::new({
        let touch_sticky = Rc::clone(&touch_sticky);
        move || {
            if !scrubbing.get_untracked() {
                return;
            }
            scrubbing.set(false);
            touch_sticky.set(None);
            let frame = timeline.playhead.get_untracked();
            timeline.scrub_pointer_up(frame);
        }
    });

    let root_class = class.unwrap_or_default();

    view! {
        <div
            class=root_class
            node_ref=scrubber_ref
            data-testid="scrubber"
            role="slider"
            attr:aria-valuemin="0"
            attr:aria-valuemax=move || timeline.frame_count.get().saturating_sub(1).to_string()
            attr:aria-valuenow=move || timeline.playhead.get().to_string()
            attr:aria-disabled=move || (!timeline.ready.get()).to_string()
            data-playhead=move || timeline.playhead.get().to_string()
            on:pointerdown={
                let touch_sticky = Rc::clone(&touch_sticky);
                move |ev| {
                    if !timeline.ready.get_untracked() {
                        return;
                    }
                    hide_hover();
                    let Some(Ok(el)) = ev.current_target().map(|t| t.dyn_into::<Element>()) else {
                        return;
                    };
                    if let Ok(html) = el.clone().dyn_into::<web_sys::HtmlElement>() {
                        let _ = html.set_pointer_capture(ev.pointer_id());
                    }
                    scrubbing.set(true);
                    let rect = el.get_bounding_client_rect();
                    let client_x = ev.client_x();
                    let x = (client_x - rect.left()).clamp(0.0, rect.width().max(1.0));
                    let Some(frame) = scrub_frame_from_x(
                        client_x,
                        rect.left(),
                        rect.width(),
                        timeline.frame_count.get_untracked(),
                    ) else {
                        touch_sticky.set(None);
                        return;
                    };
                    timeline.playhead.set(frame);
                    timeline.scrub_pointer_down(frame);
                    // Touch: arm sticky gate. Mouse/pen stay 1:1 on move.
                    if ev.pointer_type() == "touch" {
                        touch_sticky.set(Some((x, frame)));
                    } else {
                        touch_sticky.set(None);
                    }
                }
            }
            on:pointermove={
                let touch_sticky = Rc::clone(&touch_sticky);
                move |ev| {
                    let Some(Ok(el)) = ev.current_target().map(|t| t.dyn_into::<Element>()) else {
                        return;
                    };
                    if scrubbing.get_untracked() {
                        let rect = el.get_bounding_client_rect();
                        let client_x = ev.client_x();
                        let w = rect.width().max(1.0);
                        let x = (client_x - rect.left()).clamp(0.0, w);
                        let n = timeline.frame_count.get_untracked();
                        let Some(frame) = scrub_frame_from_x(client_x, rect.left(), w, n) else {
                            return;
                        };
                        if let Some((last_x, last_frame)) = touch_sticky.get() {
                            if !scrub_touch_move_accepts(last_x, x, last_frame, frame, n, w) {
                                return;
                            }
                            touch_sticky.set(Some((x, frame)));
                        }
                        timeline.playhead.set(frame);
                        timeline.scrub_pointer_move(frame);
                        return;
                    }
                    let Some(frame) = frame_from_client_x(&el, ev.client_x()) else {
                        hide_hover();
                        return;
                    };
                    let rect = el.get_bounding_client_rect();
                    let x = ev.client_x() - rect.left();
                    hover.with_value(|api| {
                        if let Some(api) = api.as_ref() {
                            api.update(x, rect.width(), frame);
                        }
                    });
                }
            }
            on:pointerleave=move |_| {
                if !scrubbing.get_untracked() {
                    hide_hover();
                }
            }
            on:pointerup={
                let finish_scrub = Rc::clone(&finish_scrub);
                move |_| {
                    finish_scrub();
                }
            }
            on:pointercancel={
                let finish_scrub = Rc::clone(&finish_scrub);
                move |_| {
                    finish_scrub();
                }
            }
        >
            {children()}
        </div>
    }
}
