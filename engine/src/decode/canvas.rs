//! Draw a [`web_sys::VideoFrame`] onto a 2D canvas.
//!
//! Buffer size is locked from layout × DPR, capped by **source display** and
//! **monitor pixels** (`screen` × DPR). Do **not** resize the canvas on every
//! present — changing `width`/`height` attributes changes the intrinsic aspect
//! and makes CSS height jitter ±1px each frame.

use wasm_bindgen::JsCast;
use web_sys::{CanvasRenderingContext2d, HtmlCanvasElement, HtmlElement, VideoFrame};

use crate::error::MediaError;

/// Placeholder bitmap matching `<Video>` initial attrs — used after clip retire
/// so the next open does not inherit the previous clip's intrinsic size.
pub const PLACEHOLDER_CANVAS_W: u32 = 320;
pub const PLACEHOLDER_CANVAS_H: u32 = 240;

/// Monitor size fallback when `window.screen` is unavailable (4K device px).
const MONITOR_FALLBACK_PX: (u32, u32) = (3840, 2160);

/// Clear prior-clip `aspect-ratio` + bitmap attrs on the **shared** stage canvas.
///
/// The DOM canvas outlives [`crate::VideoPlayer`]; without this, open B after A
/// can size from A's intrinsic box (second open of B looks fine once attrs match).
pub fn reset_canvas_layout(canvas: &HtmlCanvasElement) {
    let el: &HtmlElement = canvas.unchecked_ref();
    let _ = el.style().remove_property("aspect-ratio");
    if canvas.width() != PLACEHOLDER_CANVAS_W {
        canvas.set_width(PLACEHOLDER_CANVAS_W);
    }
    if canvas.height() != PLACEHOLDER_CANVAS_H {
        canvas.set_height(PLACEHOLDER_CANVAS_H);
    }
}

/// Pin CSS aspect-ratio and allocate the bitmap buffer once for this video.
///
/// Call on open (and on container resize via [`crate::VideoPlayer::relayout`]) —
/// not per frame. Layout follows CSS **contain** inside the parent box (like
/// `<video>` + `object-fit: contain`).
pub fn configure_canvas_for_video(
    canvas: &HtmlCanvasElement,
    src_w: u32,
    src_h: u32,
) -> Result<(), MediaError> {
    let src_w = src_w.max(1);
    let src_h = src_h.max(1);

    let el: &HtmlElement = canvas.unchecked_ref();
    el.style()
        .set_property("aspect-ratio", &format!("{src_w} / {src_h}"))
        .map_err(MediaError::from)?;
    // Flush style → layout before reading parent/canvas client_* for buffer math.
    let _ = el.offset_width();

    let (buf_w, buf_h) = buffer_size_for_layout(canvas, src_w, src_h);
    if canvas.width() != buf_w {
        canvas.set_width(buf_w);
    }
    if canvas.height() != buf_h {
        canvas.set_height(buf_h);
    }
    Ok(())
}

/// Resize the canvas to a display-sized buffer (if needed) and draw.
pub fn draw_frame_to_canvas(
    canvas: &HtmlCanvasElement,
    frame: &VideoFrame,
) -> Result<(), MediaError> {
    // Only configure when still at the placeholder size (first paint / no open hook).
    if canvas.width() <= PLACEHOLDER_CANVAS_W && canvas.height() <= PLACEHOLDER_CANVAS_H {
        configure_canvas_for_video(canvas, frame.display_width(), frame.display_height())?;
    }
    let ctx = canvas_2d(canvas)?;
    draw_frame_with_context(canvas, &ctx, frame)
}

/// Draw into the **existing** canvas buffer — never changes layout size.
pub fn draw_frame_with_context(
    canvas: &HtmlCanvasElement,
    ctx: &CanvasRenderingContext2d,
    frame: &VideoFrame,
) -> Result<(), MediaError> {
    let buf_w = canvas.width().max(1);
    let buf_h = canvas.height().max(1);
    ctx.draw_image_with_video_frame_and_dw_and_dh(frame, 0.0, 0.0, buf_w as f64, buf_h as f64)
        .map_err(MediaError::from)?;
    Ok(())
}

pub fn canvas_2d(canvas: &HtmlCanvasElement) -> Result<CanvasRenderingContext2d, MediaError> {
    canvas
        .get_context("2d")
        .map_err(MediaError::from)?
        .ok_or_else(|| MediaError::WebCodecs("canvas 2d context unavailable".into()))?
        .dyn_into::<CanvasRenderingContext2d>()
        .map_err(|_| MediaError::WebCodecs("canvas 2d cast failed".into()))
}

/// `window.devicePixelRatio`, clamped to `[1, 3]` (same as buffer layout).
pub fn device_pixel_ratio() -> f64 {
    web_sys::window()
        .map(|w| w.device_pixel_ratio())
        .unwrap_or(1.0)
        .clamp(1.0, 3.0)
}

/// Monitor size in device pixels (`screen` CSS × DPR). Fallback 4K if unavailable.
pub fn monitor_pixel_size() -> (u32, u32) {
    let Some(window) = web_sys::window() else {
        return MONITOR_FALLBACK_PX;
    };
    let dpr = device_pixel_ratio();
    let Ok(screen) = window.screen() else {
        return MONITOR_FALLBACK_PX;
    };
    let w = screen.width().unwrap_or(0) as f64;
    let h = screen.height().unwrap_or(0) as f64;
    if w < 1.0 || h < 1.0 {
        return MONITOR_FALLBACK_PX;
    }
    (
        (w * dpr).round().max(1.0) as u32,
        (h * dpr).round().max(1.0) as u32,
    )
}

/// Cap a buffer to source display and monitor (contain, keep aspect).
pub fn clamp_buffer_to_source_and_monitor(
    src_w: u32,
    src_h: u32,
    want_w: u32,
    want_h: u32,
) -> (u32, u32) {
    clamp_buffer_to_caps(src_w, src_h, want_w, want_h, monitor_pixel_size())
}

/// Pure clamp used by scrub encode + unit tests (`mon` = monitor device pixels).
pub fn clamp_buffer_to_caps(
    src_w: u32,
    src_h: u32,
    want_w: u32,
    want_h: u32,
    (mon_w, mon_h): (u32, u32),
) -> (u32, u32) {
    let src_w = src_w.max(1);
    let src_h = src_h.max(1);
    let src_aspect = src_w as f64 / src_h as f64;
    let max_w = (src_w.min(mon_w.max(1))).max(1) as f64;
    let max_h = (src_h.min(mon_h.max(1))).max(1) as f64;

    let mut w = (want_w.max(1) as f64).min(max_w);
    let mut h = (want_h.max(1) as f64).min(max_h);
    if w / h > src_aspect {
        w = (h * src_aspect).round().max(1.0);
    } else {
        h = (w / src_aspect).round().max(1.0);
    }
    if w > max_w {
        w = max_w;
        h = (w / src_aspect).round().max(1.0);
    }
    if h > max_h {
        h = max_h;
        w = (h * src_aspect).round().max(1.0);
    }
    (w as u32, h as u32)
}

/// Parent media stage when present; otherwise the canvas itself.
fn layout_box_css(canvas: &HtmlCanvasElement) -> (f64, f64) {
    if let Some(parent) = canvas.parent_element() {
        let w = parent.client_width();
        let h = parent.client_height();
        if w > 0 && h > 0 {
            return (w as f64, h as f64);
        }
    }
    let w = canvas.client_width();
    let h = canvas.client_height();
    (
        if w > 0 { w as f64 } else { 640.0 },
        if h > 0 { h as f64 } else { 360.0 },
    )
}

/// CSS contain size of the video inside `box_w` × `box_h`.
fn contain_css_size(box_w: f64, box_h: f64, src_aspect: f64) -> (f64, f64) {
    let box_w = box_w.max(1.0);
    let box_h = box_h.max(1.0);
    let box_aspect = box_w / box_h;
    if src_aspect > box_aspect {
        (box_w, box_w / src_aspect)
    } else {
        (box_h * src_aspect, box_h)
    }
}

fn buffer_size_for_layout(canvas: &HtmlCanvasElement, src_w: u32, src_h: u32) -> (u32, u32) {
    let src_aspect = src_w as f64 / src_h as f64;
    let dpr = device_pixel_ratio();

    let (box_w, box_h) = layout_box_css(canvas);
    let (css_w, css_h) = contain_css_size(box_w, box_h, src_aspect);
    let want_w = (css_w * dpr).round().max(1.0) as u32;
    let want_h = (css_h * dpr).round().max(1.0) as u32;
    clamp_buffer_to_source_and_monitor(src_w, src_h, want_w, want_h)
}
