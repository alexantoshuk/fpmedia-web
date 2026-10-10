//! Exact scrub blit plane + hover thumbs.
//!
//! - [`ScrubBlitLru`] — live `ImageBitmap` for scrub/step present, over
//!   [`AimWindow`] (evict farthest from playhead). Markers + run geometry live in
//!   [`crate::residency`]. Thumbs stay separate.

use std::collections::HashMap;
use std::num::NonZeroUsize;

use crate::residency::AimWindow;
use crate::types::FrameId;
use lru::LruCache;
use web_sys::ImageBitmap;

/// Scrub/filmstrip keyframe GOPs retained.
pub const DEFAULT_MAX_THUMB_GOPS: usize = 8;
pub const DEFAULT_MAX_THUMBS: usize = 64;

/// Live scrub blit handles. Evict farthest from playhead (not classic LRU) so
/// densify ahead cannot wipe just-presented frames (Prev stayed cold on long-GOP).
pub struct ScrubBlitLru {
    window: AimWindow<ImageBitmap>,
}

impl ScrubBlitLru {
    pub fn new(cap: usize) -> Self {
        Self {
            window: AimWindow::new(cap),
        }
    }

    /// Resize live blit window (profile policy). Evicts farthest from playhead.
    pub fn set_max(&mut self, cap: usize) {
        self.window.set_max(cap);
        while self.window.len() > self.window.max() {
            let Some((_, bmp)) = self.window.take_farthest(None) else {
                break;
            };
            bmp.close();
        }
    }

    pub fn clear(&mut self) {
        for (_, bmp) in self.window.take_all() {
            bmp.close();
        }
    }

    pub fn set_playhead(&mut self, playhead: FrameId) {
        self.window.set_aim(playhead);
    }

    /// Frames with a live `ImageBitmap` (demo Exact strip / step-ready).
    pub fn cached_frame_ids(&self) -> Vec<FrameId> {
        self.window.keys().collect()
    }

    pub fn contains(&self, frame: FrameId) -> bool {
        self.window.contains(frame)
    }

    pub fn len(&self) -> usize {
        self.window.len()
    }

    pub fn max(&self) -> usize {
        self.window.max()
    }

    /// Same refuse gate as [`Self::put`] at capacity (no bitmap alloc).
    pub fn would_evict_closer_than(&self, frame: FrameId, from: FrameId) -> bool {
        self.window.would_evict_closer_than(frame, from)
    }

    /// Closest live Exact blit to `target` (floor/ceil by `|id − target|`).
    pub fn nearest_frame(&self, target: FrameId) -> Option<FrameId> {
        self.window.nearest(target)
    }

    /// Next frame missing a live bitmap near `playhead` (biased by `dir`).
    ///
    /// Used after Play wipes blit while scrub markers stay — densify must see
    /// ImageBitmap holes, not only marker gaps. Margins = same pair as markers;
    /// see [`AimWindow::capped_next_gap`].
    pub fn next_gap_frame(
        &self,
        playhead: FrameId,
        last: FrameId,
        dir: i8,
        margin_left: u64,
        margin_right: u64,
    ) -> Option<FrameId> {
        self.window
            .capped_next_gap(playhead, last, dir, margin_left, margin_right)
    }

    pub fn get(&mut self, frame: FrameId) -> Option<&ImageBitmap> {
        self.window.get(frame)
    }

    /// Insert or replace a live bitmap. Returns `false` when refused at capacity
    /// (would only evict a closer resident — caller must close unused planes).
    pub fn put(&mut self, frame: FrameId, bitmap: ImageBitmap) -> bool {
        if !self.window.contains(frame)
            && self.window.len() >= self.window.max()
            && self
                .window
                .would_evict_closer_than(frame, self.window.aim())
        {
            bitmap.close();
            return false;
        }
        if let Some(old) = self.window.insert(frame, bitmap) {
            old.close();
            return true;
        }
        while self.window.len() > self.window.max() {
            let Some((_, bmp)) = self.window.take_farthest(Some(frame)) else {
                break;
            };
            bmp.close();
        }
        if self.window.len() > self.window.max() {
            if let Some(bmp) = self.window.remove(frame) {
                bmp.close();
            }
            return false;
        }
        true
    }
}

impl Drop for ScrubBlitLru {
    fn drop(&mut self) {
        self.clear();
    }
}

/// One GOP of hover / filmstrip thumbs at **caller-requested** bitmap size.
pub struct ThumbGop {
    pub by_time: HashMap<i64, ThumbBitmap>,
}

impl ThumbGop {
    fn close_all(&mut self) {
        for (_, thumb) in self.by_time.drain() {
            thumb.close();
        }
    }
}

/// Hover thumb plane — separate budget from Exact blit (`AGENTS.md` / product).
pub enum ThumbBitmap {
    Ready {
        bitmap: ImageBitmap,
        width: u32,
        height: u32,
    },
}

impl ThumbBitmap {
    fn close(self) {
        match self {
            Self::Ready { bitmap, .. } => bitmap.close(),
        }
    }

    fn matches_size(&self, width: u32, height: u32) -> bool {
        match self {
            Self::Ready {
                width: w,
                height: h,
                ..
            } => *w == width && *h == height,
        }
    }

    fn bitmap(&self) -> &ImageBitmap {
        match self {
            Self::Ready { bitmap, .. } => bitmap,
        }
    }
}

/// Filmstrip / hover thumbs — **not** charged to Exact `blit_mib`.
pub struct ThumbCache {
    gops: LruCache<FrameId, ThumbGop>,
    max_thumbs: usize,
}

impl ThumbCache {
    pub fn new(max_key_gops: usize, max_thumbs: usize) -> Self {
        let cap = NonZeroUsize::new(max_key_gops.max(1)).expect("max_key_gops >= 1");
        Self {
            gops: LruCache::new(cap),
            max_thumbs: max_thumbs.max(1),
        }
    }

    pub fn clear(&mut self) {
        while let Some((_, mut gop)) = self.gops.pop_lru() {
            gop.close_all();
        }
    }

    pub fn thumb_count(&self) -> usize {
        self.gops.iter().map(|(_, g)| g.by_time.len()).sum()
    }

    /// Lookup a ready thumb for `frame` under GOP `key` at the requested size.
    pub fn get(
        &mut self,
        key: FrameId,
        frame: FrameId,
        width: u32,
        height: u32,
    ) -> Option<&ImageBitmap> {
        let gop = self.gops.get_mut(&key)?;
        let thumb = gop.by_time.get(&(frame as i64))?;
        if thumb.matches_size(width, height) {
            Some(thumb.bitmap())
        } else {
            None
        }
    }

    /// Whether a ready thumb exists for `frame` under `key` at size (no LRU touch).
    pub fn contains(&self, key: FrameId, frame: FrameId, width: u32, height: u32) -> bool {
        let Some(gop) = self.gops.peek(&key) else {
            return false;
        };
        gop.by_time
            .get(&(frame as i64))
            .is_some_and(|t| t.matches_size(width, height))
    }

    /// Closest ready thumb to `target` at `width`×`height` (any GOP).
    ///
    /// Same soft-drag idea as Exact [`ScrubBlitLru::nearest_frame`] — hover should
    /// show the nearest available preview, not only an exact aim hit.
    pub fn nearest_frame(
        &self,
        target: FrameId,
        width: u32,
        height: u32,
    ) -> Option<(FrameId, FrameId)> {
        let mut best: Option<(FrameId, FrameId, u64)> = None;
        for (&key, gop) in self.gops.iter() {
            for (&ts, thumb) in &gop.by_time {
                if !thumb.matches_size(width, height) {
                    continue;
                }
                let frame = ts as FrameId;
                let dist = frame.abs_diff(target);
                let replace = match best {
                    None => true,
                    Some((_, _, d)) if dist < d => true,
                    Some((bk, bf, d)) if dist == d && (frame < bf || (frame == bf && key < bk)) => {
                        true
                    }
                    _ => false,
                };
                if replace {
                    best = Some((key, frame, dist));
                }
            }
        }
        best.map(|(k, f, _)| (k, f))
    }

    /// Insert or replace a ready thumb. Closes any previous plane for the same frame.
    pub fn put_ready(
        &mut self,
        key: FrameId,
        frame: FrameId,
        bitmap: ImageBitmap,
        width: u32,
        height: u32,
    ) {
        if !self.gops.contains(&key) {
            self.gops.put(
                key,
                ThumbGop {
                    by_time: HashMap::new(),
                },
            );
        }
        let gop = self.gops.get_mut(&key).expect("just ensured");
        if let Some(old) = gop.by_time.insert(
            frame as i64,
            ThumbBitmap::Ready {
                bitmap,
                width,
                height,
            },
        ) {
            old.close();
        }
        self.enforce_thumb_budget();
    }

    fn enforce_thumb_budget(&mut self) {
        while self.thumb_count() > self.max_thumbs && self.gops.len() > 1 {
            let victim = self.gops.iter().rev().map(|(k, _)| *k).next();
            if let Some(k) = victim {
                if let Some(mut gop) = self.gops.pop(&k) {
                    gop.close_all();
                }
            } else {
                break;
            }
        }
    }
}

impl Drop for ThumbCache {
    fn drop(&mut self) {
        self.clear();
    }
}
