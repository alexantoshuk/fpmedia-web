//! Encode `VideoFrame` → WebP bytes (scrub store) + sync `ImageBitmap` for blit.
//!
//! Densify must populate the blit LRU **synchronously**. Async `createImageBitmap(Blob)`
//! after encode left Next cold (~400ms–2s) while Prev (just presented → LRU hot) was
//! ~50ms. Use `OffscreenCanvas.transferToImageBitmap()` (sync) then WebP via HTML canvas
//! `toDataURL`. `mediabunny:` CanvasSink canvas pool — same idea, keep live bitmaps.
//!
//! Encode size = playback canvas buffer (viewport × DPR), capped by source display
//! **and** monitor pixels (`screen` × DPR). Fixed edge-640 was softer than Pause.
//!
//! Quality is intentionally low (`SCRUB_WEBP_QUALITY`): store is for scrub drag /
//! strip; settle paints decode `VideoFrame` on canvas. Chromium `1.0` = lossless.
//!
//! **Binary decode:** never `String::into_bytes()` after `atob` — use `JsString::char_code_at`.

use std::cell::RefCell;

use js_sys::{Array, JsString, Uint8Array};
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::JsFuture;
use web_sys::{
    Blob, BlobPropertyBag, CanvasRenderingContext2d, HtmlCanvasElement, ImageBitmap,
    OffscreenCanvas, OffscreenCanvasRenderingContext2d, VideoFrame,
};

use crate::error::MediaError;

/// Lossy WebP quality for Exact scrub-store chunks (drag / densify / Play trail).
/// Keep well below 1.0 — Chromium treats 1.0 as lossless and stalls encode.
pub const SCRUB_WEBP_QUALITY: f64 = 0.5;

/// One compressed Exact frame in system RAM.
#[derive(Clone, Debug)]
#[allow(dead_code)] // width/height reserved for blit scale / filmstrip
pub struct ScrubChunk {
    pub bytes: Vec<u8>,
    pub width: u32,
    pub height: u32,
    /// MIME from `toDataURL` (`image/webp` or browser fallback).
    pub mime: String,
}

/// WebP chunk + live blit handle from one densify/present ingest.
pub struct EncodedScrubFrame {
    pub chunk: ScrubChunk,
    pub bitmap: ImageBitmap,
}

/// Reusable encode surfaces (not in the DOM).
pub struct EncodeCanvas {
    /// VF → transferToImageBitmap (sync blit).
    offscreen: OffscreenCanvas,
    off_ctx: OffscreenCanvasRenderingContext2d,
    /// Bitmap → WebP `toDataURL`.
    canvas: HtmlCanvasElement,
    ctx: CanvasRenderingContext2d,
}

impl EncodeCanvas {
    pub fn new() -> Result<Self, MediaError> {
        let window =
            web_sys::window().ok_or_else(|| MediaError::WebCodecs("window unavailable".into()))?;
        let document = window
            .document()
            .ok_or_else(|| MediaError::WebCodecs("document unavailable".into()))?;
        let offscreen = OffscreenCanvas::new(1, 1).map_err(MediaError::from)?;
        let off_ctx = offscreen
            .get_context("2d")
            .map_err(MediaError::from)?
            .ok_or_else(|| MediaError::WebCodecs("offscreen 2d unavailable".into()))?
            .dyn_into::<OffscreenCanvasRenderingContext2d>()
            .map_err(|_| MediaError::WebCodecs("offscreen 2d cast failed".into()))?;
        let canvas = document
            .create_element("canvas")
            .map_err(MediaError::from)?
            .dyn_into::<HtmlCanvasElement>()
            .map_err(|_| MediaError::WebCodecs("canvas element cast failed".into()))?;
        let ctx = canvas
            .get_context("2d")
            .map_err(MediaError::from)?
            .ok_or_else(|| MediaError::WebCodecs("encode canvas 2d unavailable".into()))?
            .dyn_into::<CanvasRenderingContext2d>()
            .map_err(|_| MediaError::WebCodecs("encode canvas 2d cast failed".into()))?;
        Ok(Self {
            offscreen,
            off_ctx,
            canvas,
            ctx,
        })
    }

    /// Draw `frame` at viewport size (≤ source × monitor) → sync `ImageBitmap` + WebP.
    ///
    /// `target_w`/`target_h` are the playback canvas buffer pixels (layout × DPR).
    pub fn encode_webp_with_blit(
        &self,
        frame: &VideoFrame,
        target_w: u32,
        target_h: u32,
    ) -> Result<EncodedScrubFrame, MediaError> {
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
        // Consumes pixels — offscreen is cleared after this.
        let bitmap = self
            .offscreen
            .transfer_to_image_bitmap()
            .map_err(MediaError::from)?;

        if self.canvas.width() != width {
            self.canvas.set_width(width);
        }
        if self.canvas.height() != height {
            self.canvas.set_height(height);
        }
        self.ctx
            .draw_image_with_image_bitmap_and_dw_and_dh(
                &bitmap,
                0.0,
                0.0,
                width as f64,
                height as f64,
            )
            .map_err(MediaError::from)?;
        // Chromium treats quality 1.0 as lossless WebP — sync densify stalls.
        // Store is scrub-preview quality; settle paints decode VF on canvas.
        let data_url = self
            .canvas
            .to_data_url_with_type_and_encoder_options(
                "image/webp",
                &SCRUB_WEBP_QUALITY.into(),
            )
            .map_err(MediaError::from)?;
        let (mime, bytes) = decode_data_url(&data_url)?;
        Ok(EncodedScrubFrame {
            chunk: ScrubChunk {
                bytes,
                width,
                height,
                mime,
            },
            bitmap,
        })
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

fn decode_data_url(data_url: &str) -> Result<(String, Vec<u8>), MediaError> {
    const PREFIX: &str = "base64,";
    let (meta, b64) = data_url
        .split_once(PREFIX)
        .ok_or_else(|| MediaError::WebCodecs("data URL missing base64 payload".into()))?;
    let mime = meta
        .strip_prefix("data:")
        .unwrap_or("image/webp")
        .trim_end_matches(';')
        .split(';')
        .next()
        .unwrap_or("image/webp")
        .to_string();
    let window =
        web_sys::window().ok_or_else(|| MediaError::WebCodecs("window unavailable".into()))?;
    let binary = window.atob(b64).map_err(MediaError::from)?;
    let js = JsString::from(binary);
    let len = js.length();
    let mut bytes = Vec::with_capacity(len as usize);
    for i in 0..len {
        bytes.push(js.char_code_at(i) as u8);
    }
    if bytes.is_empty() {
        return Err(MediaError::WebCodecs("empty image encode".into()));
    }
    Ok((mime, bytes))
}

fn blob_from_chunk(chunk: &ScrubChunk) -> Result<Blob, MediaError> {
    let parts = Array::new();
    let u8 = Uint8Array::new_with_length(chunk.bytes.len() as u32);
    u8.copy_from(&chunk.bytes);
    parts.push(&u8);
    let opts = BlobPropertyBag::new();
    opts.set_type(&chunk.mime);
    Blob::new_with_u8_array_sequence_and_options(&parts, &opts).map_err(MediaError::from)
}

/// Decode chunk bytes to an [`ImageBitmap`] for canvas blit (LRU miss fallback).
pub async fn image_bitmap_from_chunk(chunk: &ScrubChunk) -> Result<ImageBitmap, MediaError> {
    let blob = blob_from_chunk(chunk)?;
    let window =
        web_sys::window().ok_or_else(|| MediaError::WebCodecs("window unavailable".into()))?;
    let promise = window
        .create_image_bitmap_with_blob(&blob)
        .map_err(MediaError::from)?;
    let bitmap = JsFuture::from(promise).await.map_err(MediaError::from)?;
    bitmap
        .dyn_into::<ImageBitmap>()
        .map_err(|_| MediaError::WebCodecs("createImageBitmap cast failed".into()))
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
