//! Isolated hover / filmstrip thumb sink (not scrub densify / play decoder).
//!
//! `mediabunny:` `CanvasSink` with small `width` + `getCanvas` /
//! `canvasesAtTimestamps` — a **separate** sink from playback. Holds ThumbCache
//! + a long-lived 3rd [`WebCodecsDecoder`] (`create_thumb` = scrub isolation).

use std::cell::{Cell, RefCell};

use web_sys::ImageBitmap;

use super::frame_cache::{DEFAULT_MAX_THUMB_GOPS, DEFAULT_MAX_THUMBS, ThumbCache};
use super::scrub_blit::{BlitCanvas, BlitCanvasSlot};
use super::webcodecs::WebCodecsDecoder;
use crate::error::MediaError;
use crate::types::FrameId;

/// Lazy thumb plane: cache + cancel epoch + dedicated decoder (Phase 3).
pub struct ThumbPreview {
    cache: RefCell<ThumbCache>,
    /// Bumped to cancel in-flight Exact downscale / thumb decode.
    epoch: Cell<u64>,
    decoder: RefCell<Option<WebCodecsDecoder>>,
    /// Own blit surface — never share densify `encode_canvas` (RefCell re-entry).
    blit: BlitCanvasSlot,
    /// HEVC HW-only: refuse a 3rd decoder for this clip (Exact downscale only).
    decode_blocked: Cell<bool>,
    /// Exclusive decode worker (latest-wins via [`Self::pending_aim`]).
    busy: Cell<bool>,
    /// Newest aim while a worker is running (presentation frame).
    pending_aim: Cell<Option<FrameId>>,
    /// Latest cursor aim — paint nearest-to-this after decode (not a stale decode id).
    hover_aim: Cell<FrameId>,
}

impl ThumbPreview {
    pub fn new() -> Self {
        Self {
            cache: RefCell::new(ThumbCache::new(DEFAULT_MAX_THUMB_GOPS, DEFAULT_MAX_THUMBS)),
            epoch: Cell::new(0),
            decoder: RefCell::new(None),
            blit: BlitCanvasSlot::default(),
            decode_blocked: Cell::new(false),
            busy: Cell::new(false),
            pending_aim: Cell::new(None),
            hover_aim: Cell::new(0),
        }
    }

    pub fn cancel(&self) {
        self.epoch.set(self.epoch.get().wrapping_add(1));
        self.pending_aim.set(None);
    }

    pub fn generation(&self) -> u64 {
        self.epoch.get()
    }

    pub fn decode_blocked(&self) -> bool {
        self.decode_blocked.get()
    }

    pub fn set_decode_blocked(&self, blocked: bool) {
        self.decode_blocked.set(blocked);
    }

    /// Drop cache + decoder (open / close / `ThumbPolicy::Off`).
    pub fn clear(&self) {
        self.cancel();
        if let Ok(mut cache) = self.cache.try_borrow_mut() {
            cache.clear();
        }
        if let Ok(mut slot) = self.decoder.try_borrow_mut()
            && let Some(mut d) = slot.take()
        {
            let _ = d.reset();
        }
        self.decode_blocked.set(false);
        self.busy.set(false);
    }

    /// Lookup + paint while the cache borrow is held (`ImageBitmap` is not `Clone`).
    pub fn with_bitmap<R>(
        &self,
        key: FrameId,
        frame: FrameId,
        width: u32,
        height: u32,
        f: impl FnOnce(&ImageBitmap) -> R,
    ) -> Option<R> {
        let Ok(mut cache) = self.cache.try_borrow_mut() else {
            return None;
        };
        cache.get(key, frame, width, height).map(f)
    }

    /// Nearest cached thumb id at size (scrubber-style soft preview).
    pub fn nearest_frame(&self, target: FrameId, width: u32, height: u32) -> Option<FrameId> {
        let Ok(cache) = self.cache.try_borrow() else {
            return None;
        };
        cache.nearest_frame(target, width, height).map(|(_, f)| f)
    }

    /// Paint nearest thumb to `target` at size; returns the painted frame id.
    pub fn with_nearest_bitmap<R>(
        &self,
        target: FrameId,
        width: u32,
        height: u32,
        f: impl FnOnce(&ImageBitmap) -> R,
    ) -> Option<(FrameId, R)> {
        let Ok(mut cache) = self.cache.try_borrow_mut() else {
            return None;
        };
        let (key, frame) = cache.nearest_frame(target, width, height)?;
        let out = cache.get(key, frame, width, height).map(f)?;
        Some((frame, out))
    }

    pub fn put_ready(
        &self,
        key: FrameId,
        frame: FrameId,
        bitmap: ImageBitmap,
        width: u32,
        height: u32,
    ) {
        if let Ok(mut cache) = self.cache.try_borrow_mut() {
            cache.put_ready(key, frame, bitmap, width, height);
        } else {
            bitmap.close();
        }
    }

    pub fn set_hover_aim(&self, aim: FrameId) {
        self.hover_aim.set(aim);
    }

    pub fn hover_aim(&self) -> FrameId {
        self.hover_aim.get()
    }

    pub fn take_decoder(&self) -> Option<WebCodecsDecoder> {
        self.decoder.borrow_mut().take()
    }

    pub fn put_decoder(&self, decoder: WebCodecsDecoder) {
        *self.decoder.borrow_mut() = Some(decoder);
    }

    pub fn with_blit<R>(
        &self,
        f: impl FnOnce(&BlitCanvas) -> Result<R, MediaError>,
    ) -> Result<R, MediaError> {
        self.blit.with_blit(f)
    }

    /// Record latest aim; return `true` if this caller should run the decode loop.
    pub fn claim_decode(&self, aim: FrameId) -> bool {
        self.hover_aim.set(aim);
        self.pending_aim.set(Some(aim));
        if self.busy.get() {
            return false;
        }
        self.busy.set(true);
        true
    }

    pub fn take_pending_aim(&self) -> Option<FrameId> {
        self.pending_aim.take()
    }

    pub fn has_pending_aim(&self) -> bool {
        self.pending_aim.get().is_some()
    }

    pub fn end_decode(&self) {
        self.busy.set(false);
    }
}

impl Default for ThumbPreview {
    fn default() -> Self {
        Self::new()
    }
}
