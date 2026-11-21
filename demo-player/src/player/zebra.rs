use super::format::zebra_pair_hex;
use fpmedia_web_components::ScrubberCtx;
use fpmedia_web_components::helpers::{
    FRAME_ZEBRA_MIN_SLOT_PX, frame_zebra_step, odd_frame_runs, strip_canvas_buffer_width,
};
use leptos::html::{Canvas, Div};
use leptos::prelude::*;
use leptos_use::use_resize_observer;
use wasm_bindgen::JsCast;

/// Default zebra amplitude (~odd `#1a1a1a` under previous hard-coded paint).
pub(super) const FRAME_ZEBRA_CONTRAST: u8 = 16;

/// Frame zebra on the progress track (demo chrome). Continuous `w/n` float
/// `fillRect` slots (same [`frame_slot`] as tip); off when step &lt; [`FRAME_ZEBRA_MIN_SLOT_PX`].
///
/// Measure a host **div** (not the canvas — `set_width` lies to RO/CSS). Below the
/// gate the canvas is unmounted (`Show`) so a wide bitmap cannot mush into subpixels.
#[component]
pub(super) fn FrameZebra(contrast: u8) -> impl IntoView {
    let ctx = use_context::<ScrubberCtx>().expect("FrameZebra must be used inside <Scrubber>");
    let video = ctx.timeline;
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
pub(super) fn paint_frame_zebra(
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
