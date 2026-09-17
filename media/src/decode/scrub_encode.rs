//! VideoFrame -> sync `ImageBitmap` for densify blit + resident store markers.
//!
//! Densify populates [`crate::decode::frame_cache::ScrubBlitLru`] synchronously via
//! `OffscreenCanvas.transferToImageBitmap()`. The scrub store holds **markers only**
//! (gap geometry / green strip) -- canvas present never inflates a still image.
//! `mediabunny:` CanvasSink canvas pool -- keep live bitmaps for present.
//!
//! Bitmap size = playback canvas buffer (viewport x DPR), capped by source display
//! and monitor pixels (`screen` x DPR).

use std::cell::RefCell;

use wasm_bindgen::JsCast;
use web_sys::{
    CanvasRenderingContext2d, HtmlCanvasElement, ImageBitmap, OffscreenCanvas,
    OffscreenCanvasRenderingContext2d, VideoFrame,
};

use crate::error::MediaError;

/// Resident densify marker in [`crate::decode::frame_cache::ScrubChunkStore`].
///
/// Presence-only — canvas present uses [`crate::decode::frame_cache::ScrubBlitLru`].
#[derive(Clone, Copy, Debug, Default)]
pub struct ScrubChunk;

/// Reusable VF -> `ImageBitmap` surface (not in the DOM).
pub struct EncodeCanvas {
    offscreen: OffscreenCanvas,
    off_ctx: OffscreenCanvasRenderingContext2d,
}

impl EncodeCanvas {
    pub fn new() -> Result<Self, MediaError> {
        let offscreen = OffscreenCanvas::new(1, 1).map_err(MediaError::from)?;
        let off_ctx = offscreen
            .get_context("2d")
            .map_err(MediaError::from)?
            .ok_or_else(|| MediaError::WebCodecs("offscreen 2d unavailable".into()))?
            .dyn_into::<OffscreenCanvasRenderingContext2d>()
            .map_err(|_| MediaError::WebCodecs("offscreen 2d cast failed".into()))?;
        Ok(Self { offscreen, off_ctx })
    }

    /// VF -> sized OffscreenCanvas -> sync `ImageBitmap`.
    pub fn frame_to_bitmap(
        &self,
        frame: &VideoFrame,
        target_w: u32,
        target_h: u32,
    ) -> Result<(ImageBitmap, u32, u32), MediaError> {
        let src_w = frame.display_width().max(1);
        let src_h = frame.display_height().max(1);
        let (width, height) = crate::decode::canvas::clamp_buffer_to_source_and_monitor(
            src_w, src_h, target_w, target_h,
        );
        if self.offscreen.width() != width {
            self.offscreen.set_width(width);
        }
        if self.offscreen.height() != height {
            self.offscreen.set_height(height);
        }
        self.off_ctx
            .draw_image_with_video_frame_and_dw_and_dh(frame, 0.0, 0.0, width as f64, height as f64)
            .map_err(MediaError::from)?;
        let bitmap = self
            .offscreen
            .transfer_to_image_bitmap()
            .map_err(MediaError::from)?;
        Ok((bitmap, width, height))
    }
}

/// Lazy [`EncodeCanvas`] holder for `Shared`.
#[derive(Default)]
pub struct EncodeCanvasSlot(RefCell<Option<EncodeCanvas>>);

impl EncodeCanvasSlot {
    pub fn with_encoder<R>(
        &self,
        f: impl FnOnce(&EncodeCanvas) -> Result<R, MediaError>,
    ) -> Result<R, MediaError> {
        let mut slot = self.0.borrow_mut();
        if slot.is_none() {
            *slot = Some(EncodeCanvas::new()?);
        }
        f(slot.as_ref().expect("just inserted"))
    }
}

/// Draw an [`ImageBitmap`] into the playback canvas buffer (no layout resize).
pub fn draw_image_bitmap(
    canvas: &HtmlCanvasElement,
    ctx: &CanvasRenderingContext2d,
    bitmap: &ImageBitmap,
) -> Result<(), MediaError> {
    let buf_w = canvas.width().max(1);
    let buf_h = canvas.height().max(1);
    ctx.draw_image_with_image_bitmap_and_dw_and_dh(bitmap, 0.0, 0.0, buf_w as f64, buf_h as f64)
        .map_err(MediaError::from)?;
    Ok(())
}
