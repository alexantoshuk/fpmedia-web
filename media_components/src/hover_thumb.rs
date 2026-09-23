//! Frame.io-style hover thumb chrome (not part of [`crate::Video`]).
//!
//! Hosts wire this next to a scrubber track and call [`HoverThumbApi`] from
//! pointer handlers. Engine paint stays on [`crate::VideoRef`]; this module owns
//! canvas, transform, coalesce, and cancel-on-leave.
//!
//! Feature `thumbs` (default on): real paint. Soft stubs when off.
//! Play+pause hover; host hides only while scrubbing (`mediabunny:` separate
//! thumb `CanvasSink`).

use std::rc::Rc;

use leptos::html::Canvas;
use leptos::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::spawn_local;
use web_sys::{HtmlCanvasElement, HtmlElement, window};

use crate::VideoRef;
use crate::helpers::{hover_thumb_left, hover_thumb_transform_css};

/// Default CSS / canvas box (matches `.wm-thumb-preview` in `style/input.css`).
pub const DEFAULT_HOVER_THUMB_MAX_W: u32 = 160;
pub const DEFAULT_HOVER_THUMB_MAX_H: u32 = 90;

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

/// Coalesce Exact downscales: debounce then one aim. Do **not** cancel on every
/// pointermove — only [`VideoRef::cancel_hover_thumb`] on leave / scrub.
fn defer_hover_paint(video: VideoRef, frame: u32, dest: HtmlCanvasElement, max_w: u32, max_h: u32) {
    #[cfg(not(feature = "thumbs"))]
    {
        let _ = (video, frame, dest, max_w, max_h);
        return;
    }
    #[cfg(feature = "thumbs")]
    {
        thread_local! {
            static PENDING: std::cell::RefCell<Option<(u32, HtmlCanvasElement, u32, u32)>> =
                const { std::cell::RefCell::new(None) };
            static SCHEDULED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
        }
        PENDING.with(|p| {
            *p.borrow_mut() = Some((frame, dest, max_w, max_h));
        });
        if SCHEDULED.with(|s| s.get()) {
            return;
        }
        SCHEDULED.with(|s| s.set(true));
        spawn_local(async move {
            let _ = wasm_bindgen_futures::JsFuture::from(js_sys::Promise::new(
                &mut |resolve, _reject| {
                    if let Some(w) = window() {
                        let _ =
                            w.set_timeout_with_callback_and_timeout_and_arguments_0(&resolve, 80);
                    } else {
                        let _ = resolve.call0(&wasm_bindgen::JsValue::UNDEFINED);
                    }
                },
            ))
            .await;
            if let Some((frame, dest, max_w, max_h)) = PENDING.with(|p| p.borrow_mut().take()) {
                video.paint_hover_thumb(frame, dest, max_w, max_h);
            }
            SCHEDULED.with(|s| s.set(false));
            if let Some((frame, dest, max_w, max_h)) = PENDING.with(|p| p.borrow().clone()) {
                defer_hover_paint(video, frame, dest, max_w, max_h);
            }
        });
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
    let epoch = StoredValue::new_local(0u64);

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
                defer_hover_paint(video, frame, canvas, max_w, max_h);
            }
        })
    };

    api.set_value(Some(HoverThumbApi {
        hide: Rc::clone(&hide),
        update,
    }));

    on_cleanup(move || {
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
