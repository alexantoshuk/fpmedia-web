//! Isolated hover / filmstrip thumb sink (not scrub densify / play decoder).
//!
//! `mediabunny:` `CanvasSink` with small `width` + `getCanvas` /
//! `canvasesAtTimestamps` — a **separate** sink from playback. Phase 1 is
//! read-only (ThumbCache + Exact downscale); Phase 3 adds a dedicated decoder.

use std::cell::{Cell, RefCell};

use web_sys::ImageBitmap;

use super::frame_cache::{DEFAULT_MAX_THUMB_GOPS, DEFAULT_MAX_THUMBS, ThumbCache};
use crate::types::FrameId;

/// Lazy thumb plane: cache + cancel generation (decoder lands in Phase 3).
pub struct ThumbPreview {
    cache: RefCell<ThumbCache>,
    /// Bumped to cancel in-flight Exact downscale / future decode.
    epoch: Cell<u64>,
}

impl ThumbPreview {
    pub fn new() -> Self {
        Self {
            cache: RefCell::new(ThumbCache::new(DEFAULT_MAX_THUMB_GOPS, DEFAULT_MAX_THUMBS)),
            epoch: Cell::new(0),
        }
    }

    pub fn cancel(&self) {
        self.epoch.set(self.epoch.get().wrapping_add(1));
    }

    pub fn generation(&self) -> u64 {
        self.epoch.get()
    }

    pub fn clear(&self) {
        if let Ok(mut cache) = self.cache.try_borrow_mut() {
            cache.clear();
        }
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
}

impl Default for ThumbPreview {
    fn default() -> Self {
        Self::new()
    }
}
