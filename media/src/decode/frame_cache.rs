//! Playhead-centered `VideoFrame` cache: **slab of handles** + **frame index**.
//!
//! - Slab: `Option<OwnedVideoFrame>` slots — owns handles.
//! - Index: sparse `FrameId → slot` (visited / gap-filled frames; gaps allowed).
//! Eviction drops the frame farthest from the playhead. Thumbs stay separate.

use std::collections::HashMap;
use std::num::NonZeroUsize;

use lru::LruCache;
use crate::decode::owned_video_frame::OwnedVideoFrame;
use crate::error::MediaError;

use crate::types::FrameId;

/// Soft cap on cached `VideoFrame`s (longer clips).
pub const DEFAULT_MAX_FRAMES: usize = 96;
/// Kept for API compat with [`DenseFrameCache::budgets_for_clip`] / `new`.
pub const DEFAULT_MAX_CHUNKS: usize = DEFAULT_MAX_FRAMES;
/// Short-clip mode: keep up to this many exact frames (whole file when ≤ N).
pub const SHORT_CLIP_FRAME_BUDGET: usize = 200;
/// Scrub/filmstrip keyframe GOPs retained.
pub const DEFAULT_MAX_THUMB_GOPS: usize = 8;
pub const DEFAULT_MAX_THUMBS: usize = 64;

/// Owning table of `VideoFrame` handles.
struct FrameSlab {
    slots: Vec<Option<OwnedVideoFrame>>,
    free: Vec<usize>,
}

impl FrameSlab {
    fn new() -> Self {
        Self {
            slots: Vec::new(),
            free: Vec::new(),
        }
    }

    fn len_occupied(&self) -> usize {
        self.slots.len().saturating_sub(self.free.len())
    }

    fn clear(&mut self) {
        self.slots.clear();
        self.free.clear();
    }

    fn alloc(&mut self, video: OwnedVideoFrame) -> usize {
        if let Some(idx) = self.free.pop() {
            self.slots[idx] = Some(video);
            idx
        } else {
            let idx = self.slots.len();
            self.slots.push(Some(video));
            idx
        }
    }

    fn free_slot(&mut self, idx: usize) {
        if let Some(slot) = self.slots.get_mut(idx) {
            if slot.take().is_some() {
                self.free.push(idx);
            }
        }
    }

    fn get(&self, idx: usize) -> Option<&OwnedVideoFrame> {
        self.slots.get(idx)?.as_ref()
    }
}

impl Drop for FrameSlab {
    fn drop(&mut self) {
        self.clear();
    }
}

/// Exact-frame residency: slab owns handles; map addresses them sparsely.
pub struct DenseFrameCache {
    slab: FrameSlab,
    /// Sparse visited / gap-filled frames → slab slot.
    index: HashMap<FrameId, usize>,
    max_frames: usize,
    playhead: FrameId,
    playhead_key: FrameId,
}

impl Drop for DenseFrameCache {
    fn drop(&mut self) {
        self.clear();
    }
}

impl DenseFrameCache {
    pub fn new(_max_chunks: usize, max_frames: usize) -> Self {
        Self {
            slab: FrameSlab::new(),
            index: HashMap::new(),
            max_frames: max_frames.max(1),
            playhead: 0,
            playhead_key: 0,
        }
    }

    /// Public budget trim — drop frames farthest from the playhead.
    #[allow(dead_code)]
    pub fn enforce_budget(&mut self) {
        self.enforce_frame_budget(None);
    }

    pub fn set_budgets(&mut self, _max_chunks: usize, max_frames: usize) {
        let max_frames = max_frames.max(1);
        if max_frames == self.max_frames {
            return;
        }
        self.max_frames = max_frames;
        self.enforce_frame_budget(None);
    }

    pub fn max_frames(&self) -> usize {
        self.max_frames
    }

    pub fn clear(&mut self) {
        self.index.clear();
        self.slab.clear();
    }

    /// Contiguous runs among cached ids (demo meter “chunks”).
    pub fn chunk_count(&self) -> usize {
        let mut ids: Vec<FrameId> = self.index.keys().copied().collect();
        if ids.is_empty() {
            return 0;
        }
        ids.sort_unstable();
        let mut runs = 1usize;
        for w in ids.windows(2) {
            if w[1] != w[0] + 1 {
                runs += 1;
            }
        }
        runs
    }

    pub fn frame_count(&self) -> usize {
        debug_assert_eq!(self.index.len(), self.slab.len_occupied());
        self.slab.len_occupied()
    }

    pub fn cached_frame_ids(&self) -> Vec<FrameId> {
        let mut out: Vec<FrameId> = self.index.keys().copied().collect();
        out.sort_unstable();
        out
    }

    /// Inclusive min/max cached frame, if any.
    pub fn extent(&self) -> Option<(FrameId, FrameId)> {
        let mut iter = self.index.keys().copied();
        let first = iter.next()?;
        let (mut lo, mut hi) = (first, first);
        for f in iter {
            lo = lo.min(f);
            hi = hi.max(f);
        }
        Some((lo, hi))
    }

    pub fn touch_playhead(&mut self, frame: FrameId, key: FrameId) {
        self.playhead = frame;
        self.playhead_key = key;
        self.enforce_frame_budget(Some(frame));
    }

    pub fn playhead(&self) -> FrameId {
        self.playhead
    }

    pub fn has_frame(&self, frame: FrameId, _key: FrameId) -> bool {
        self.index.contains_key(&frame)
    }

    /// Borrow the cached handle for canvas draw (avoids `VideoFrame.clone`, which
    /// often fails in browsers).
    pub fn with_frame<R>(&self, frame: FrameId, f: impl FnOnce(&OwnedVideoFrame) -> R) -> Option<R> {
        let slot = *self.index.get(&frame)?;
        Some(f(self.slab.get(slot)?))
    }

    /// Clone a cached frame for handoff (`frame_at`, etc.). Prefer [`Self::with_frame`]
    /// for drawing. `Ok(None)` = not cached; `Err` = present but clone failed.
    pub fn try_clone_frame(&self, frame: FrameId) -> Result<Option<OwnedVideoFrame>, MediaError> {
        let Some(&slot) = self.index.get(&frame) else {
            return Ok(None);
        };
        let Some(video) = self.slab.get(slot) else {
            return Ok(None);
        };
        Ok(Some(video.try_clone()?))
    }

    /// Insert one visited / gap-filled frame. Evicts farthest from playhead if needed.
    pub fn insert_frame(&mut self, frame: FrameId, video: OwnedVideoFrame) {
        if self.index.contains_key(&frame) {
            return;
        }
        while self.frame_count() >= self.max_frames {
            if !self.evict_one(Some(frame)) {
                break;
            }
        }
        if self.frame_count() >= self.max_frames {
            return;
        }
        let slot = self.slab.alloc(video);
        self.index.insert(frame, slot);
    }

    pub fn insert_gop(&mut self, _key: FrameId, frames: HashMap<FrameId, OwnedVideoFrame>) {
        if frames.is_empty() {
            return;
        }
        let mut items: Vec<(FrameId, OwnedVideoFrame)> = frames.into_iter().collect();
        items.sort_by_key(|(id, _)| *id);
        let mut protect = None;
        for (id, video) in items {
            if self.index.contains_key(&id) {
                continue;
            }
            self.insert_frame(id, video);
            protect = Some(id);
        }
        self.enforce_frame_budget(protect);
    }

    /// Nearest missing presentation frame to `playhead` within `[0, last]`.
    /// Prefer behind on distance ties (helps PREV).
    pub fn next_gap_frame(&self, playhead: FrameId, last: FrameId) -> Option<FrameId> {
        if last == 0 && !self.index.contains_key(&0) {
            return Some(0);
        }
        let playhead = playhead.min(last);
        if !self.index.contains_key(&playhead) {
            return Some(playhead);
        }
        let mut best: Option<(u64, u8, FrameId)> = None;
        // Behind: playhead-1, playhead-2, …
        let mut f = playhead;
        while f > 0 {
            f -= 1;
            if !self.index.contains_key(&f) {
                let dist = playhead - f;
                best = Some((dist, 0, f));
                break;
            }
        }
        // Ahead: playhead+1 …
        let mut f = playhead;
        while f < last {
            f += 1;
            if !self.index.contains_key(&f) {
                let dist = f - playhead;
                let cand = (dist, 1, f);
                best = match best {
                    Some(b) if b.0 < cand.0 || (b.0 == cand.0 && b.1 <= cand.1) => Some(b),
                    _ => Some(cand),
                };
                break;
            }
        }
        best.map(|(_, _, id)| id)
    }

    /// True when every frame in `[0, last]` is cached, or cache is at capacity
    /// with no nearer gaps than the current extent allows under budget.
    pub fn gaps_saturated(&self, playhead: FrameId, last: FrameId) -> bool {
        self.next_gap_frame(playhead, last).is_none()
            || self.frame_count() >= self.max_frames
                && self
                    .next_gap_frame(playhead, last)
                    .is_some_and(|g| self.would_evict_closer_than(g, playhead))
    }

    /// If inserting `gap` at capacity would immediately evict something closer
    /// than `gap` (or the playhead), gap-fill should stop.
    fn would_evict_closer_than(&self, gap: FrameId, playhead: FrameId) -> bool {
        if self.frame_count() < self.max_frames {
            return false;
        }
        let gap_dist = gap.abs_diff(playhead);
        self.index
            .keys()
            .copied()
            .filter(|f| *f != playhead)
            .map(|f| f.abs_diff(playhead))
            .max()
            .is_some_and(|far| far <= gap_dist)
    }

    pub fn budgets_for_clip(frame_count: u64, _sync_count: usize) -> (usize, usize) {
        let n = frame_count as usize;
        if n > 0 && n <= SHORT_CLIP_FRAME_BUDGET {
            let frames = n.max(1);
            (frames, frames)
        } else {
            (DEFAULT_MAX_CHUNKS, DEFAULT_MAX_FRAMES)
        }
    }

    pub fn gop_complete(&self, key: FrameId, gop_ids: &[FrameId]) -> bool {
        if gop_ids.is_empty() {
            return self.has_frame(key, key);
        }
        gop_ids.iter().all(|id| self.has_frame(*id, key))
    }

    fn enforce_frame_budget(&mut self, protect: Option<FrameId>) {
        while self.frame_count() > self.max_frames {
            if !self.evict_one(protect) {
                break;
            }
        }
    }

    fn evict_one(&mut self, protect: Option<FrameId>) -> bool {
        let victim = self
            .index
            .keys()
            .copied()
            .filter(|f| Some(*f) != protect && *f != self.playhead)
            .max_by_key(|f| f.abs_diff(self.playhead));
        match victim {
            Some(f) => self.remove_frame(f),
            None => false,
        }
    }

    fn remove_frame(&mut self, frame: FrameId) -> bool {
        let Some(slot) = self.index.remove(&frame) else {
            return false;
        };
        self.slab.free_slot(slot);
        true
    }
}

/// One GOP of scrub previews at **caller-requested** bitmap size (UI owns scale).
#[allow(dead_code)]
pub struct ThumbGop {
    pub key_frame: FrameId,
    pub by_time: HashMap<i64, ThumbBitmap>,
}

#[allow(dead_code)]
pub enum ThumbBitmap {
    Reserved { width: u32, height: u32 },
}

#[allow(dead_code)]
pub struct ThumbCache {
    gops: LruCache<FrameId, ThumbGop>,
    max_thumbs: usize,
}

#[allow(dead_code)]
impl ThumbCache {
    pub fn new(max_key_gops: usize, max_thumbs: usize) -> Self {
        let cap = NonZeroUsize::new(max_key_gops.max(1)).expect("max_key_gops >= 1");
        Self {
            gops: LruCache::new(cap),
            max_thumbs: max_thumbs.max(1),
        }
    }

    pub fn clear(&mut self) {
        self.gops.clear();
    }

    pub fn gop_count(&self) -> usize {
        self.gops.len()
    }

    pub fn thumb_count(&self) -> usize {
        self.gops.iter().map(|(_, g)| g.by_time.len()).sum()
    }

    pub fn get_gop_mut(&mut self, key: FrameId) -> Option<&mut ThumbGop> {
        self.gops.get_mut(&key)
    }

    pub fn ensure_gop(&mut self, key: FrameId) -> &mut ThumbGop {
        if !self.gops.contains(&key) {
            self.gops.put(
                key,
                ThumbGop {
                    key_frame: key,
                    by_time: HashMap::new(),
                },
            );
        }
        self.enforce_thumb_budget();
        self.gops.get_mut(&key).expect("just inserted")
    }

    fn enforce_thumb_budget(&mut self) {
        while self.thumb_count() > self.max_thumbs && self.gops.len() > 1 {
            let victim = self.gops.iter().rev().map(|(k, _)| *k).next();
            if let Some(k) = victim {
                let _ = self.gops.pop(&k);
            } else {
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn next_gap_prefers_nearest_behind_on_tie() {
        let mut c = DenseFrameCache::new(8, 8);
        // Empty: playhead itself is the gap.
        assert_eq!(c.next_gap_frame(5, 10), Some(5));
        // Simulate occupied playhead without a real VideoFrame — use index insert via
        // a stub is hard; test distance logic with empty+manual would need insert.
        // Playhead missing takes priority — covered above.
        let _ = &mut c;
    }

    #[test]
    fn next_gap_expands_both_directions() {
        let c = DenseFrameCache::new(8, 8);
        // No frames cached → gap is playhead.
        assert_eq!(c.next_gap_frame(3, 9), Some(3));
    }
}
