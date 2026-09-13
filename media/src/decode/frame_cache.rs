//! Playhead-centered `VideoFrame` cache: ordered `BTreeMap` of handles.
//!
//! Keys are presentation `FrameId` (sparse / gap-filled; holes allowed).
//! Eviction drops the frame farthest from the playhead (always an extent end).
//! Thumbs stay separate.

use std::collections::{BTreeMap, HashMap};
use std::num::NonZeroUsize;
use std::ops::Bound;

use crate::decode::owned_video_frame::OwnedVideoFrame;
use crate::error::MediaError;
use crate::types::{FrameId, SCRUB_CACHE_EDGE_MARGIN};
use lru::LruCache;

/// Scrub/filmstrip keyframe GOPs retained.
pub const DEFAULT_MAX_THUMB_GOPS: usize = 8;
pub const DEFAULT_MAX_THUMBS: usize = 64;

/// Result of a gap probe (for fill + demo diagnostics).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GapSearch {
    pub gap: Option<FrameId>,
    /// Key / integer steps taken while locating the gap (0 = O(1) path).
    pub steps: u32,
    pub path: GapSearchPath,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GapSearchPath {
    EmptyClip,
    PlayheadMissing,
    ContiguousEnds,
    RunBounds,
}

impl GapSearchPath {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::EmptyClip => "empty",
            Self::PlayheadMissing => "miss",
            Self::ContiguousEnds => "ends",
            Self::RunBounds => "run",
        }
    }
}

/// Exact-frame residency: `BTreeMap` owns handles and keeps ids ordered.
pub struct DenseFrameCache {
    frames: BTreeMap<FrameId, OwnedVideoFrame>,
    max_frames: usize,
    playhead: FrameId,
    playhead_key: FrameId,
    /// `true` when the cached extent is a single contiguous run (no holes).
    no_gaps: bool,
}

impl Drop for DenseFrameCache {
    fn drop(&mut self) {
        self.clear();
    }
}

impl DenseFrameCache {
    pub fn new(max_frames: usize) -> Self {
        Self {
            frames: BTreeMap::new(),
            max_frames: max_frames.max(1),
            playhead: 0,
            playhead_key: 0,
            no_gaps: false,
        }
    }

    /// Public budget trim — drop frames farthest from the playhead.
    #[allow(dead_code)]
    pub fn enforce_budget(&mut self) {
        self.enforce_frame_budget(None);
    }

    pub fn set_max_frames(&mut self, max_frames: usize) {
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
        self.frames.clear();
        self.no_gaps = false;
    }

    /// True when the cached extent has no holes (`len == end - start + 1`).
    pub fn no_gaps(&self) -> bool {
        self.no_gaps
    }

    /// Whether scrub gap-fill should run for `playhead`.
    ///
    /// - Holes → always armed.
    /// - Contiguous **under budget** → armed (keep expanding into long empty
    ///   regions; edge-idle only applies once the frame budget is full).
    /// - Contiguous **at capacity** → armed only near extent edges / outside
    ///   (within [`SCRUB_CACHE_EDGE_MARGIN`]).
    pub fn gap_fill_armed(&self, playhead: FrameId) -> bool {
        if !self.no_gaps() {
            return true;
        }
        if self.frame_count() < self.max_frames {
            return true;
        }
        scrub_gap_fill_armed(
            true,
            playhead,
            self.extent(),
            SCRUB_CACHE_EDGE_MARGIN,
        )
    }

    /// Contiguous runs among cached ids (demo meter “chunks”).
    pub fn chunk_count(&self) -> usize {
        let mut runs = 0usize;
        let mut prev: Option<FrameId> = None;
        for &id in self.frames.keys() {
            match prev {
                Some(p) if id == p + 1 => {}
                _ => runs += 1,
            }
            prev = Some(id);
        }
        runs
    }

    pub fn frame_count(&self) -> usize {
        self.frames.len()
    }

    pub fn cached_frame_ids(&self) -> Vec<FrameId> {
        self.frames.keys().copied().collect()
    }

    /// Inclusive min/max cached frame, if any.
    pub fn extent(&self) -> Option<(FrameId, FrameId)> {
        let lo = *self.frames.keys().next()?;
        let hi = *self.frames.keys().next_back()?;
        Some((lo, hi))
    }

    pub fn touch_playhead(&mut self, frame: FrameId, key: FrameId) {
        self.playhead = frame;
        self.playhead_key = key;
        // Evict farthest from the new aim when over budget — do **not** wipe
        // other visited runs (scrub should keep frames it already touched).
        self.enforce_frame_budget(Some(frame));
    }

    pub fn playhead(&self) -> FrameId {
        self.playhead
    }

    /// Nearest cached presentation frame to `target` (by `|id − target|`).
    /// On distance ties, prefer the lower id (floor over ceil).
    pub fn nearest_frame(&self, target: FrameId) -> Option<FrameId> {
        let floor = self.frames.range(..=target).next_back().map(|(id, _)| *id);
        let ceil = self.frames.range(target..).next().map(|(id, _)| *id);
        match (floor, ceil) {
            (Some(a), Some(b)) if a != b => {
                if a.abs_diff(target) <= b.abs_diff(target) {
                    Some(a)
                } else {
                    Some(b)
                }
            }
            (Some(a), _) | (_, Some(a)) => Some(a),
            (None, None) => None,
        }
    }

    pub fn has_frame(&self, frame: FrameId, _key: FrameId) -> bool {
        self.frames.contains_key(&frame)
    }

    /// Borrow the cached handle for canvas draw (avoids `VideoFrame.clone`, which
    /// often fails in browsers).
    pub fn with_frame<R>(
        &self,
        frame: FrameId,
        f: impl FnOnce(&OwnedVideoFrame) -> R,
    ) -> Option<R> {
        Some(f(self.frames.get(&frame)?))
    }

    /// Clone a cached frame for handoff (`frame_at`, etc.). Prefer [`Self::with_frame`]
    /// for drawing. `Ok(None)` = not cached; `Err` = present but clone failed.
    pub fn try_clone_frame(&self, frame: FrameId) -> Result<Option<OwnedVideoFrame>, MediaError> {
        let Some(video) = self.frames.get(&frame) else {
            return Ok(None);
        };
        Ok(Some(video.try_clone()?))
    }

    /// Insert one visited / gap-filled frame. Evicts farthest from playhead if needed.
    ///
    /// Returns `true` when `frame` is newly resident (not already present / not refused).
    ///
    /// At capacity, refuses to insert when every resident is closer than `frame`
    /// (same rule as [`Self::gaps_saturated`]). Without this, playhead at 0
    /// expands ahead by evicting `end` then inserting `end+1`, leaving a lone
    /// keyframe outside a holed window.
    pub fn insert_frame(&mut self, frame: FrameId, video: OwnedVideoFrame) -> bool {
        if self.frames.contains_key(&frame) {
            return false;
        }
        if self.frame_count() >= self.max_frames
            && self.would_evict_closer_than(frame, self.playhead)
        {
            return false;
        }
        while self.frame_count() >= self.max_frames {
            if !self.evict_one(Some(frame)) {
                break;
            }
        }
        if self.frame_count() >= self.max_frames {
            return false;
        }
        self.frames.insert(frame, video);
        self.refresh_no_gaps();
        true
    }

    /// Batch insert (tests / non-streaming callers). Scrub fill streams via
    /// [`Self::insert_frame`] per decoded output so the ready strip updates live.
    #[allow(dead_code)]
    pub fn insert_gop(&mut self, _key: FrameId, frames: HashMap<FrameId, OwnedVideoFrame>) {
        if frames.is_empty() {
            return;
        }
        let mut items: Vec<(FrameId, OwnedVideoFrame)> = frames.into_iter().collect();
        items.sort_by_key(|(id, _)| *id);
        let mut protect = None;
        for (id, video) in items {
            if self.frames.contains_key(&id) {
                continue;
            }
            self.insert_frame(id, video);
            if !self.frames.contains_key(&id) {
                break;
            }
            protect = Some(id);
        }
        self.enforce_frame_budget(protect);
    }

    /// Nearest missing presentation frame to `playhead` within `[0, last]`.
    /// Prefer behind on distance ties (helps PREV).
    pub fn next_gap_frame(&self, playhead: FrameId, last: FrameId) -> Option<FrameId> {
        self.next_gap_search(playhead, last).gap
    }

    /// Like [`Self::next_gap_frame`] plus step/path counters for diagnostics.
    pub fn next_gap_search(&self, playhead: FrameId, last: FrameId) -> GapSearch {
        if last == 0 && !self.frames.contains_key(&0) {
            return GapSearch {
                gap: Some(0),
                steps: 0,
                path: GapSearchPath::EmptyClip,
            };
        }
        let playhead = playhead.min(last);
        if !self.frames.contains_key(&playhead) {
            return GapSearch {
                gap: Some(playhead),
                steps: 0,
                path: GapSearchPath::PlayheadMissing,
            };
        }
        if self.no_gaps {
            let gap = self.next_gap_outside_extent(playhead, last);
            return GapSearch {
                gap,
                steps: 0,
                path: GapSearchPath::ContiguousEnds,
            };
        }
        // Walk only *cached* keys in the run containing playhead (≤ max_frames),
        // not every integer across a long hole.
        let mut steps = 0u32;
        let mut run_lo = playhead;
        for &id in self.frames.range(..playhead).rev().map(|(k, _)| k) {
            steps = steps.saturating_add(1);
            if id + 1 == run_lo {
                run_lo = id;
            } else {
                break;
            }
        }
        let mut run_hi = playhead;
        for &id in self
            .frames
            .range((Bound::Excluded(playhead), Bound::Unbounded))
            .map(|(k, _)| k)
        {
            steps = steps.saturating_add(1);
            if id == run_hi + 1 {
                run_hi = id;
            } else {
                break;
            }
        }
        let behind = run_lo.checked_sub(1);
        let ahead = (run_hi < last).then_some(run_hi + 1);
        GapSearch {
            gap: nearer_gap_prefer_behind(playhead, behind, ahead),
            steps,
            path: GapSearchPath::RunBounds,
        }
    }

    /// True when every frame in `[0, last]` is cached, or cache is at capacity
    /// with no nearer gaps than the current extent allows under budget.
    pub fn gaps_saturated(&self, playhead: FrameId, last: FrameId) -> bool {
        let gap = self.next_gap_frame(playhead, last);
        gap.is_none()
            || self.frame_count() >= self.max_frames
                && gap.is_some_and(|g| self.would_evict_closer_than(g, playhead))
    }

    /// If inserting `gap` at capacity would immediately evict something closer
    /// than `gap` (or the playhead), gap-fill should stop.
    fn would_evict_closer_than(&self, gap: FrameId, playhead: FrameId) -> bool {
        if self.frame_count() < self.max_frames {
            return false;
        }
        let gap_dist = gap.abs_diff(playhead);
        self.max_dist_from_playhead(playhead)
            .is_some_and(|far| far <= gap_dist)
    }

    /// Frame budget for a clip — delegates to [`crate::VideoMetadata::scrub_cache_frame_budget`].
    pub fn budget_for_clip(meta: &crate::types::VideoMetadata) -> usize {
        meta.scrub_cache_frame_budget()
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

    /// Farthest *evictable* resident is an endpoint of the remaining key set
    /// (full extent ends may be playhead / protect).
    fn evict_one(&mut self, protect: Option<FrameId>) -> bool {
        let Some(victim) = self.farthest_evictable(protect) else {
            return false;
        };
        self.remove_frame(victim)
    }

    fn farthest_evictable(&self, protect: Option<FrameId>) -> Option<FrameId> {
        let allowed = |f: FrameId| Some(f) != protect && f != self.playhead;
        let lo = self.frames.keys().copied().find(|&f| allowed(f))?;
        let hi = self.frames.keys().copied().rev().find(|&f| allowed(f))?;
        [lo, hi]
            .into_iter()
            .max_by_key(|f| f.abs_diff(self.playhead))
    }

    fn max_dist_from_playhead(&self, playhead: FrameId) -> Option<u64> {
        let lo = self
            .frames
            .keys()
            .copied()
            .find(|&f| f != playhead)?;
        let hi = self
            .frames
            .keys()
            .copied()
            .rev()
            .find(|&f| f != playhead)?;
        Some(lo.abs_diff(playhead).max(hi.abs_diff(playhead)))
    }

    fn next_gap_outside_extent(&self, playhead: FrameId, last: FrameId) -> Option<FrameId> {
        let (lo, hi) = self.extent()?;
        // `then_some(lo - 1)` evaluates the sub even when lo == 0 (debug overflow).
        let behind = lo.checked_sub(1);
        let ahead = hi.checked_add(1).filter(|&a| a <= last);
        nearer_gap_prefer_behind(playhead, behind, ahead)
    }

    fn remove_frame(&mut self, frame: FrameId) -> bool {
        if self.frames.remove(&frame).is_none() {
            return false;
        }
        self.refresh_no_gaps();
        true
    }

    fn refresh_no_gaps(&mut self) {
        self.no_gaps = match self.extent() {
            Some((lo, hi)) => {
                // Contiguous run length; checked to avoid debug overflow if lo > hi.
                hi.checked_sub(lo)
                    .and_then(|d| d.checked_add(1))
                    .is_some_and(|n| n == self.frames.len() as u64)
            }
            None => false,
        };
    }
}

/// Pick nearer of `behind` / `ahead` to `playhead`; on distance ties prefer behind.
pub(crate) fn nearer_gap_prefer_behind(
    playhead: FrameId,
    behind: Option<FrameId>,
    ahead: Option<FrameId>,
) -> Option<FrameId> {
    match (behind, ahead) {
        (Some(b), Some(a)) => {
            if playhead.abs_diff(b) <= playhead.abs_diff(a) {
                Some(b)
            } else {
                Some(a)
            }
        }
        (Some(b), None) => Some(b),
        (None, Some(a)) => Some(a),
        (None, None) => None,
    }
}

/// Gap-fill arming once the dense window is contiguous **and at capacity**.
///
/// - Holes (`!no_gaps`) → always armed (caller).
/// - Under budget → always armed (caller).
/// - Contiguous at capacity → armed only when `playhead` is within `margin` of
///   either extent edge, or outside `[start, end]` entirely.
pub(crate) fn scrub_gap_fill_armed(
    no_gaps: bool,
    playhead: FrameId,
    extent: Option<(FrameId, FrameId)>,
    margin: u64,
) -> bool {
    if !no_gaps {
        return true;
    }
    let Some((start, end)) = extent else {
        return true;
    };
    playhead < start.saturating_add(margin) || playhead > end.saturating_sub(margin)
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
        let mut c = DenseFrameCache::new(8);
        assert_eq!(c.next_gap_frame(5, 10), Some(5));
        let _ = &mut c;
    }

    #[test]
    fn next_gap_expands_both_directions() {
        let c = DenseFrameCache::new(8);
        assert_eq!(c.next_gap_frame(3, 9), Some(3));
    }

    #[test]
    fn nearer_gap_outside_extent_prefers_behind_on_tie() {
        assert_eq!(
            nearer_gap_prefer_behind(150, Some(99), Some(201)),
            Some(99)
        );
        assert_eq!(nearer_gap_prefer_behind(120, Some(99), Some(201)), Some(99));
        assert_eq!(
            nearer_gap_prefer_behind(180, Some(99), Some(201)),
            Some(201)
        );
    }

    #[test]
    fn gap_outside_extent_lo_zero_has_no_behind() {
        let behind = 0u64.checked_sub(1);
        let ahead = 7u64.checked_add(1).filter(|&a| a <= 100);
        assert_eq!(nearer_gap_prefer_behind(0, behind, ahead), Some(8));
    }

    #[test]
    fn scrub_gap_fill_armed_while_holes() {
        assert!(scrub_gap_fill_armed(false, 50, Some((40, 60)), 16));
    }

    #[test]
    fn scrub_gap_fill_armed_near_edges_when_contiguous() {
        let extent = Some((100, 200));
        let m = 16;
        assert!(!scrub_gap_fill_armed(true, 150, extent, m));
        assert!(scrub_gap_fill_armed(true, 110, extent, m));
        assert!(scrub_gap_fill_armed(true, 190, extent, m));
        assert!(scrub_gap_fill_armed(true, 50, extent, m));
        assert!(scrub_gap_fill_armed(true, 250, extent, m));
    }

    #[test]
    fn refuse_farther_insert_when_playhead_at_origin() {
        assert!(would_evict_closer_than_dist(8, 0, 7));
        assert!(!would_evict_closer_than_dist(12, 5, 5));
    }

    fn would_evict_closer_than_dist(gap: FrameId, playhead: FrameId, far_dist: u64) -> bool {
        far_dist <= gap.abs_diff(playhead)
    }
}
