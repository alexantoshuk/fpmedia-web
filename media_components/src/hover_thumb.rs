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
use web_sys::{HtmlCanvasElement, HtmlElement};

use crate::VideoRef;
use crate::helpers::{hover_thumb_left, hover_thumb_transform_css};

/// Default CSS / canvas box (matches `.wm-thumb-preview` in `style/input.css`).
pub const DEFAULT_HOVER_THUMB_MAX_W: u32 = 160;
pub const DEFAULT_HOVER_THUMB_MAX_H: u32 = 90;

/// Coalesce window before deferred `paint_hover_thumb` (ms). Engine aim-decode
/// floor is separate (`PLAY_THUMB_AIM_DECODE_MIN_MS` = 800).
const DEFER_HOVER_PAINT_MS: i32 = 150;

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

#[cfg(feature = "thumbs")]
thread_local! {
    static HOVER_PAINT_PENDING: std::cell::RefCell<Option<(u32, HtmlCanvasElement, u32, u32)>> =
        const { std::cell::RefCell::new(None) };
    static HOVER_PAINT_SCHEDULED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Drop coalesced aim so clip switch / leave cannot paint a stale frame.
fn clear_deferred_hover_paint() {
    #[cfg(feature = "thumbs")]
    {
        HOVER_PAINT_PENDING.with(|p| {
            *p.borrow_mut() = None;
        });
    }
}

/// Coalesce Exact downscales: debounce then one aim. Do **not** cancel on every
/// pointermove — only [`VideoRef::cancel_hover_thumb`] on leave / scrub / open.
fn defer_hover_paint(video: VideoRef, frame: u32, dest: HtmlCanvasElement, max_w: u32, max_h: u32) {
    #[cfg(not(feature = "thumbs"))]
    {
        let _ = (video, frame, dest, max_w, max_h);
        return;
    }
    #[cfg(feature = "thumbs")]
    {
        HOVER_PAINT_PENDING.with(|p| {
            *p.borrow_mut() = Some((frame, dest, max_w, max_h));
        });
        if HOVER_PAINT_SCHEDULED.with(|s| s.get()) {
            return;
        }
        HOVER_PAINT_SCHEDULED.with(|s| s.set(true));
        spawn_local(async move {
            webmedia::timer::sleep_ms(DEFER_HOVER_PAINT_MS).await;
            let pending = HOVER_PAINT_PENDING.with(|p| p.borrow_mut().take());
            if let Some((frame, dest, max_w, max_h)) = pending {
                // Clip switch sets ready=false — do not paint into a retiring sink.
                if video.ready.get_untracked() {
                    video.paint_hover_thumb(frame, dest, max_w, max_h);
                }
            }
            HOVER_PAINT_SCHEDULED.with(|s| s.set(false));
            if video.ready.get_untracked()
                && let Some((frame, dest, max_w, max_h)) =
                    HOVER_PAINT_PENDING.with(|p| p.borrow().clone())
            {
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
        clear_deferred_hover_paint();
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
        // `hide` is `Rc` (not Send) — clear TLS + engine cancel without capturing it.
        clear_deferred_hover_paint();
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
