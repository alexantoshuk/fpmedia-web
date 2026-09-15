//! Playhead-centered `VideoFrame` cache: ordered `BTreeMap` of handles.
//!
//! - [`DenseFrameCache`] — residency, aim, budget / eviction.
//! - [`FrameRunIndex`] — contiguous-run geometry + nearest-gap search.
//!   Eviction drops the frame farthest from the playhead. Thumbs stay separate.

use std::cell::Cell;
use std::collections::btree_map::Entry;
use std::collections::{BTreeMap, HashMap};
use std::num::NonZeroUsize;
use std::ops::Bound;

use crate::decode::owned_video_frame::OwnedVideoFrame;
use crate::error::MediaError;
use crate::types::{FrameId, SCRUB_CACHE_EDGE_MARGIN, nearer_gap_with_dir};
use lru::LruCache;

/// Scrub/filmstrip keyframe GOPs retained.
pub const DEFAULT_MAX_THUMB_GOPS: usize = 8;
pub const DEFAULT_MAX_THUMBS: usize = 64;

/// Contiguous-run geometry and nearest-gap search over a `FrameId` key set.
///
/// Does not own frames — only the optional cached run around the cache aim.
pub struct FrameRunIndex {
    /// Inclusive run containing the dense-cache aim, when known.
    playhead_run: Cell<Option<(FrameId, FrameId)>>,
}

impl Default for FrameRunIndex {
    fn default() -> Self {
        Self::new()
    }
}

impl FrameRunIndex {
    pub fn new() -> Self {
        Self {
            playhead_run: Cell::new(None),
        }
    }

    pub fn clear(&self) {
        self.playhead_run.set(None);
    }

    /// Aim moved: keep the cache only if the new aim stays inside the run.
    pub fn on_aim_change(&self, new_aim: FrameId) {
        if let Some((lo, hi)) = self.playhead_run.get()
            && (new_aim < lo || new_aim > hi)
        {
            self.playhead_run.set(None);
        }
    }

    /// After a successful insert into `frames`.
    pub fn note_insert<V>(&self, frames: &BTreeMap<FrameId, V>, aim: FrameId, frame: FrameId) {
        let Some((lo, hi)) = self.playhead_run.get() else {
            return;
        };
        if !frames.contains_key(&aim) {
            self.playhead_run.set(None);
            return;
        }
        if frame + 1 == lo {
            let mut new_lo = frame;
            while new_lo > 0 && frames.contains_key(&(new_lo - 1)) {
                new_lo -= 1;
            }
            self.playhead_run.set(Some((new_lo, hi)));
        } else if frame == hi + 1 {
            let mut new_hi = frame;
            while frames.contains_key(&(new_hi + 1)) {
                new_hi += 1;
            }
            self.playhead_run.set(Some((lo, new_hi)));
        }
        // Outside this run / already covered — leave cache as-is.
    }

    /// After removing `frame` from the key set.
    pub fn note_remove(&self, aim: FrameId, frame: FrameId) {
        let Some((lo, hi)) = self.playhead_run.get() else {
            return;
        };
        if frame < lo || frame > hi {
            return;
        }
        if frame == aim || (lo < frame && frame < hi) {
            self.playhead_run.set(None);
            return;
        }
        if frame == lo {
            if lo == hi {
                self.playhead_run.set(None);
            } else {
                self.playhead_run.set(Some((lo + 1, hi)));
            }
        } else if frame == hi {
            if lo == hi {
                self.playhead_run.set(None);
            } else {
                self.playhead_run.set(Some((lo, hi - 1)));
            }
        }
    }

    /// Whole key set is a single solid run `[lo, hi]` (and `aim` is inside it).
    pub fn set_solid_run(&self, lo: FrameId, hi: FrameId) {
        self.playhead_run.set(Some((lo, hi)));
    }

    /// Nearest missing id to `playhead` within `[0, last]`.
    ///
    /// `dir > 0` / `dir < 0` → predictive densify (prefer ahead / behind).
    /// `dir == 0` → min distance, tie → behind.
    #[allow(clippy::too_many_arguments)] // gap search needs aim/playhead/extent/dir knobs
    pub fn search<V>(
        &self,
        frames: &BTreeMap<FrameId, V>,
        aim: FrameId,
        playhead: FrameId,
        last: FrameId,
        no_gaps: bool,
        extent: Option<(FrameId, FrameId)>,
        dir: i8,
    ) -> Option<FrameId> {
        if last == 0 && !frames.contains_key(&0) {
            return Some(0);
        }
        let playhead = playhead.min(last);
        if !frames.contains_key(&playhead) {
            return Some(playhead);
        }
        if no_gaps {
            return next_gap_outside_extent(playhead, last, extent, dir);
        }

        // Do not use `.then_some(playhead - 1)` — the subtraction is eager and
        // overflows when playhead == 0 (debug panic / release wrap).
        let behind_adj = playhead.checked_sub(1).filter(|b| !frames.contains_key(b));
        let ahead_adj = (playhead < last)
            .then(|| playhead + 1)
            .filter(|a| !frames.contains_key(a));
        if behind_adj.is_some() && ahead_adj.is_some() {
            return nearer_gap_with_dir(playhead, behind_adj, ahead_adj, dir);
        }
        if behind_adj.is_some() || ahead_adj.is_some() {
            let (run_lo, run_hi) = self.run_bounds(frames, aim, playhead);
            let behind = behind_adj.or_else(|| run_lo.checked_sub(1));
            let ahead = ahead_adj.or_else(|| (run_hi < last).then_some(run_hi + 1));
            return nearer_gap_with_dir(playhead, behind, ahead, dir);
        }

        let (run_lo, run_hi) = self.run_bounds(frames, aim, playhead);
        let behind = run_lo.checked_sub(1);
        let ahead = (run_hi < last).then_some(run_hi + 1);
        nearer_gap_with_dir(playhead, behind, ahead, dir)
    }

    fn run_bounds<V>(
        &self,
        frames: &BTreeMap<FrameId, V>,
        aim: FrameId,
        playhead: FrameId,
    ) -> (FrameId, FrameId) {
        if playhead == aim
            && let Some((lo, hi)) = self.playhead_run.get()
            && playhead >= lo
            && playhead <= hi
        {
            return (lo, hi);
        }
        let (lo, hi) = walk_run_bounds(frames, playhead);
        if playhead == aim {
            self.playhead_run.set(Some((lo, hi)));
        }
        (lo, hi)
    }
}

fn walk_run_bounds<V>(frames: &BTreeMap<FrameId, V>, playhead: FrameId) -> (FrameId, FrameId) {
    let mut run_lo = playhead;
    for &id in frames.range(..playhead).rev().map(|(k, _)| k) {
        if id + 1 == run_lo {
            run_lo = id;
        } else {
            break;
        }
    }
    let mut run_hi = playhead;
    for &id in frames
        .range((Bound::Excluded(playhead), Bound::Unbounded))
        .map(|(k, _)| k)
    {
        if id == run_hi + 1 {
            run_hi = id;
        } else {
            break;
        }
    }
    (run_lo, run_hi)
}

fn next_gap_outside_extent(
    playhead: FrameId,
    last: FrameId,
    extent: Option<(FrameId, FrameId)>,
    dir: i8,
) -> Option<FrameId> {
    let (lo, hi) = extent?;
    let behind = lo.checked_sub(1);
    let ahead = hi.checked_add(1).filter(|&a| a <= last);
    nearer_gap_with_dir(playhead, behind, ahead, dir)
}

/// Exact-frame residency: `BTreeMap` owns handles; [`FrameRunIndex`] tracks gaps.
pub struct DenseFrameCache {
    frames: BTreeMap<FrameId, OwnedVideoFrame>,
    runs: FrameRunIndex,
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
            runs: FrameRunIndex::new(),
            max_frames: max_frames.max(1),
            playhead: 0,
            playhead_key: 0,
            no_gaps: false,
        }
    }

    /// Gap / run index (tests and advanced callers).
    #[allow(dead_code)]
    pub fn runs(&self) -> &FrameRunIndex {
        &self.runs
    }

    /// Public budget trim — drop frames farthest from the playhead.
    #[allow(dead_code)]
    pub fn enforce_budget(&mut self) {
        self.enforce_frame_budget(None);
    }

    /// Drop farthest-from-`aim` residents until at most `max` frames remain.
    ///
    /// Available for pool-pressure recovery; do **not** call on every settle —
    /// that wiped the scrub strip on pointer-up.
    #[allow(dead_code)]
    pub fn trim_to_near(&mut self, aim: FrameId, max: usize) {
        let max = max.max(1);
        self.playhead = aim;
        while self.frame_count() > max {
            if !self.evict_one(Some(aim)) {
                break;
            }
        }
        self.refresh_no_gaps();
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
        self.runs.clear();
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
        scrub_gap_fill_armed(true, playhead, self.extent(), SCRUB_CACHE_EDGE_MARGIN)
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
        if frame != self.playhead {
            self.runs.on_aim_change(frame);
        }
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
    ///
    /// Does **not** replace an existing resident — densify/park keep the first
    /// copy. Play/seek presents that must match canvas use [`Self::put_presented_frame`].
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
        self.runs.note_insert(&self.frames, self.playhead, frame);
        self.refresh_no_gaps();
        true
    }

    /// Store a frame that was just drawn to canvas so Pause→Seek/Prev/Next can
    /// re-present the same bitmap.
    ///
    /// Unlike [`Self::insert_frame`]: **replaces** an existing resident, and will
    /// not refuse for budget (evicts farther frames, then stale playhead if needed).
    pub fn put_presented_frame(&mut self, frame: FrameId, video: OwnedVideoFrame) {
        if let Entry::Occupied(mut e) = self.frames.entry(frame) {
            drop(e.insert(video));
            return;
        }
        while self.frame_count() >= self.max_frames {
            if self.evict_one(Some(frame)) {
                continue;
            }
            // Presented frame must win: aim may still be the previous playhead.
            if self.playhead != frame && self.remove_frame(self.playhead) {
                continue;
            }
            let other = self.frames.keys().copied().find(|&f| f != frame);
            if let Some(victim) = other {
                self.remove_frame(victim);
                continue;
            }
            break;
        }
        if self.frame_count() >= self.max_frames {
            // Capacity 0 / single-slot pathological — drop rather than leak.
            drop(video);
            return;
        }
        self.frames.insert(frame, video);
        self.runs.note_insert(&self.frames, self.playhead, frame);
        self.refresh_no_gaps();
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
    ///
    /// `dir`: scrub motion (`+1` ahead / `-1` behind / `0` neutral). See
    /// [`crate::nearer_gap_with_dir`].
    pub fn next_gap_frame(&self, playhead: FrameId, last: FrameId, dir: i8) -> Option<FrameId> {
        self.runs.search(
            &self.frames,
            self.playhead,
            playhead,
            last,
            self.no_gaps,
            self.extent(),
            dir,
        )
    }

    /// True when every frame in `[0, last]` is cached, or cache is at capacity
    /// with no nearer gaps than the current extent allows under budget.
    pub fn gaps_saturated(&self, playhead: FrameId, last: FrameId, dir: i8) -> bool {
        let gap = self.next_gap_frame(playhead, last, dir);
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
        let lo = self.frames.keys().copied().find(|&f| f != playhead)?;
        let hi = self.frames.keys().copied().rev().find(|&f| f != playhead)?;
        Some(lo.abs_diff(playhead).max(hi.abs_diff(playhead)))
    }

    /// Move a resident frame out (play prime lifts dense → presentation queue).
    ///
    /// Prefer this over [`Self::try_clone_frame`] — browser `VideoFrame.clone` often
    /// fails; play handoff should **move**, then pause parks back via insert.
    pub fn take_frame(&mut self, frame: FrameId) -> Option<OwnedVideoFrame> {
        let video = self.frames.remove(&frame)?;
        self.runs.note_remove(self.playhead, frame);
        self.refresh_no_gaps();
        Some(video)
    }

    fn remove_frame(&mut self, frame: FrameId) -> bool {
        self.take_frame(frame).is_some()
    }

    fn refresh_no_gaps(&mut self) {
        self.no_gaps = match self.extent() {
            Some((lo, hi)) => {
                let solid = hi
                    .checked_sub(lo)
                    .and_then(|d| d.checked_add(1))
                    .is_some_and(|n| n == self.frames.len() as u64);
                if solid && self.frames.contains_key(&self.playhead) {
                    self.runs.set_solid_run(lo, hi);
                }
                solid
            }
            None => {
                self.runs.clear();
                false
            }
        };
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
        assert_eq!(c.next_gap_frame(5, 10, 0), Some(5));
        let _ = &mut c;
    }

    #[test]
    fn next_gap_expands_both_directions() {
        let c = DenseFrameCache::new(8);
        assert_eq!(c.next_gap_frame(3, 9, 0), Some(3));
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
