//! Draw a [`web_sys::VideoFrame`] onto a 2D canvas.

use wasm_bindgen::JsCast;
use web_sys::{HtmlCanvasElement, VideoFrame};

use crate::error::MediaError;

/// Resize the canvas to the frame's coded size (if needed) and draw.
pub fn draw_frame_to_canvas(
    canvas: &HtmlCanvasElement,
    frame: &VideoFrame,
) -> Result<(), MediaError> {
    let width = frame.display_width();
    let height = frame.display_height();
    if canvas.width() != width {
        canvas.set_width(width);
    }
    if canvas.height() != height {
        canvas.set_height(height);
    }

    let ctx = canvas
        .get_context("2d")
        .map_err(MediaError::from)?
        .ok_or_else(|| MediaError::WebCodecs("canvas 2d context unavailable".into()))?
        .dyn_into::<web_sys::CanvasRenderingContext2d>()
        .map_err(|_| MediaError::WebCodecs("canvas 2d cast failed".into()))?;

    ctx.draw_image_with_video_frame(frame, 0.0, 0.0)
        .map_err(MediaError::from)?;
    Ok(())
}
