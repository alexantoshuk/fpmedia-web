//! Draw a [`web_sys::VideoFrame`] onto a 2D canvas.
//!
//! Buffer size is locked from layout × DPR (capped). Do **not** resize the
//! canvas on every present — changing `width`/`height` attributes changes the
//! intrinsic aspect and makes CSS height jitter ±1px each frame.

use wasm_bindgen::JsCast;
use web_sys::{CanvasRenderingContext2d, HtmlCanvasElement, HtmlElement, VideoFrame};

use crate::error::MediaError;

/// Longest canvas edge we will allocate (device pixels).
const PLAYBACK_MAX_EDGE: u32 = 1280;

/// Pin CSS aspect-ratio and allocate the bitmap buffer once for this video.
///
/// Call on open (and on container resize via [`crate::WebPlayer::relayout`]) —
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
    if canvas.width() <= 320 && canvas.height() <= 240 {
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
    let dpr = web_sys::window()
        .map(|w| w.device_pixel_ratio())
        .unwrap_or(1.0)
        .clamp(1.0, 2.0);

    let (box_w, box_h) = layout_box_css(canvas);
    let (css_w, css_h) = contain_css_size(box_w, box_h, src_aspect);

    let max_edge = src_w.max(src_h).min(PLAYBACK_MAX_EDGE) as f64;
    let mut buf_w = (css_w * dpr).round().clamp(1.0, max_edge);
    let mut buf_h = (css_h * dpr).round().clamp(1.0, max_edge);
    // Keep buffer aspect aligned with source after clamping.
    if buf_w / buf_h > src_aspect {
        buf_w = (buf_h * src_aspect).round().max(1.0);
    } else {
        buf_h = (buf_w / src_aspect).round().max(1.0);
    }
    (buf_w as u32, buf_h as u32)
}
