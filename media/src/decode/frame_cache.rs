//! Playhead-centered Exact scrub residency (markers + blit LRU) + thumbs.
//!
//! - [`ScrubChunkStore`] — densify resident markers (gap geometry / Exact strip).
//! - [`ScrubBlitLru`] — live `ImageBitmap` for scrub/step present.
//! - [`FrameRunIndex`] — contiguous-run geometry + nearest-gap search.
//!   Eviction drops the frame farthest from the playhead. Thumbs stay separate.

use std::cell::Cell;
use std::collections::btree_map::Entry;
use std::collections::{BTreeMap, HashMap};
use std::num::NonZeroUsize;
use std::ops::Bound;

use crate::decode::scrub_blit::ScrubChunk;
use crate::types::{FrameId, nearer_gap_with_dir, scrub_gap_fill_armed};
use lru::LruCache;
use web_sys::ImageBitmap;

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

/// Exact scrub residency: densify markers in RAM + paired blit LRU (not live VFs).
pub struct ScrubChunkStore {
    chunks: BTreeMap<FrameId, ScrubChunk>,
    runs: FrameRunIndex,
    max_frames: usize,
    playhead: FrameId,
    no_gaps: bool,
}

impl Drop for ScrubChunkStore {
    fn drop(&mut self) {
        self.clear();
    }
}

impl ScrubChunkStore {
    pub fn new(max_frames: usize) -> Self {
        Self {
            chunks: BTreeMap::new(),
            runs: FrameRunIndex::new(),
            max_frames: max_frames.max(1),
            playhead: 0,
            no_gaps: false,
        }
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
        self.chunks.clear();
        self.no_gaps = false;
        self.runs.clear();
    }

    pub fn no_gaps(&self) -> bool {
        self.no_gaps
    }

    pub fn gap_fill_armed(
        &self,
        playhead: FrameId,
        last: FrameId,
        margin_left: u64,
        margin_right: u64,
    ) -> bool {
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
            last,
            margin_left,
            margin_right,
        )
    }

    pub fn chunk_count(&self) -> usize {
        let mut runs = 0usize;
        let mut prev: Option<FrameId> = None;
        for &id in self.chunks.keys() {
            match prev {
                Some(p) if id == p + 1 => {}
                _ => runs += 1,
            }
            prev = Some(id);
        }
        runs
    }

    pub fn frame_count(&self) -> usize {
        self.chunks.len()
    }

    /// Marker ids (gap geometry). Demo strip dim `Covered` ticks use these.
    pub fn cached_frame_ids(&self) -> Vec<FrameId> {
        self.chunks.keys().copied().collect()
    }

    pub fn extent(&self) -> Option<(FrameId, FrameId)> {
        let lo = *self.chunks.keys().next()?;
        let hi = *self.chunks.keys().next_back()?;
        Some((lo, hi))
    }

    pub fn touch_playhead(&mut self, frame: FrameId) {
        if frame != self.playhead {
            self.runs.on_aim_change(frame);
        }
        self.playhead = frame;
        self.enforce_frame_budget(Some(frame));
    }

    pub fn playhead(&self) -> FrameId {
        self.playhead
    }

    pub fn nearest_frame(&self, target: FrameId) -> Option<FrameId> {
        let floor = self.chunks.range(..=target).next_back().map(|(id, _)| *id);
        let ceil = self.chunks.range(target..).next().map(|(id, _)| *id);
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

    pub fn has_frame(&self, frame: FrameId) -> bool {
        self.chunks.contains_key(&frame)
    }

    #[allow(dead_code)] // chunk inspect / future strip decode
    pub fn get(&self, frame: FrameId) -> Option<&ScrubChunk> {
        self.chunks.get(&frame)
    }

    /// Insert without replacing. Returns `true` when newly resident.
    pub fn insert_chunk(&mut self, frame: FrameId, chunk: ScrubChunk) -> bool {
        if self.chunks.contains_key(&frame) {
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
        self.chunks.insert(frame, chunk);
        self.runs.note_insert(&self.chunks, self.playhead, frame);
        self.refresh_no_gaps();
        true
    }

    /// Replace-or-insert (presented frame must match canvas).
    pub fn put_chunk(&mut self, frame: FrameId, chunk: ScrubChunk) {
        if let Entry::Occupied(mut e) = self.chunks.entry(frame) {
            e.insert(chunk);
            return;
        }
        while self.frame_count() >= self.max_frames {
            if self.evict_one(Some(frame)) {
                continue;
            }
            if self.playhead != frame && self.remove_frame(self.playhead) {
                continue;
            }
            let other = self.chunks.keys().copied().find(|&f| f != frame);
            if let Some(victim) = other {
                self.remove_frame(victim);
                continue;
            }
            break;
        }
        if self.frame_count() >= self.max_frames {
            return;
        }
        self.chunks.insert(frame, chunk);
        self.runs.note_insert(&self.chunks, self.playhead, frame);
        self.refresh_no_gaps();
    }

    /// Drop one resident (bad encode / blit failure).
    #[allow(dead_code)] // kept for ingest rollback / future Exact-only failure paths
    pub fn remove(&mut self, frame: FrameId) -> bool {
        self.remove_frame(frame)
    }

    pub fn next_gap_frame(&self, playhead: FrameId, last: FrameId, dir: i8) -> Option<FrameId> {
        self.runs.search(
            &self.chunks,
            self.playhead,
            playhead,
            last,
            self.no_gaps,
            self.extent(),
            dir,
        )
    }

    pub fn gaps_saturated(&self, playhead: FrameId, last: FrameId, dir: i8) -> bool {
        let gap = self.next_gap_frame(playhead, last, dir);
        gap.is_none()
            || self.frame_count() >= self.max_frames
                && gap.is_some_and(|g| self.would_evict_closer_than(g, playhead))
    }

    pub fn gop_complete(&self, gop_ids: &[FrameId]) -> bool {
        if gop_ids.is_empty() {
            return false;
        }
        gop_ids.iter().all(|id| self.has_frame(*id))
    }

    fn would_evict_closer_than(&self, gap: FrameId, playhead: FrameId) -> bool {
        if self.frame_count() < self.max_frames {
            return false;
        }
        let gap_dist = gap.abs_diff(playhead);
        self.max_dist_from_playhead(playhead)
            .is_some_and(|far| far <= gap_dist)
    }

    fn enforce_frame_budget(&mut self, protect: Option<FrameId>) {
        while self.frame_count() > self.max_frames {
            if !self.evict_one(protect) {
                break;
            }
        }
    }

    fn evict_one(&mut self, protect: Option<FrameId>) -> bool {
        let Some(victim) = self.farthest_evictable(protect) else {
            return false;
        };
        self.remove_frame(victim)
    }

    fn farthest_evictable(&self, protect: Option<FrameId>) -> Option<FrameId> {
        let allowed = |f: FrameId| Some(f) != protect && f != self.playhead;
        let lo = self.chunks.keys().copied().find(|&f| allowed(f))?;
        let hi = self.chunks.keys().copied().rev().find(|&f| allowed(f))?;
        [lo, hi]
            .into_iter()
            .max_by_key(|f| f.abs_diff(self.playhead))
    }

    fn max_dist_from_playhead(&self, playhead: FrameId) -> Option<u64> {
        let lo = self.chunks.keys().copied().find(|&f| f != playhead)?;
        let hi = self.chunks.keys().copied().rev().find(|&f| f != playhead)?;
        Some(lo.abs_diff(playhead).max(hi.abs_diff(playhead)))
    }

    fn remove_frame(&mut self, frame: FrameId) -> bool {
        if self.chunks.remove(&frame).is_none() {
            return false;
        }
        self.runs.note_remove(self.playhead, frame);
        self.refresh_no_gaps();
        true
    }

    fn refresh_no_gaps(&mut self) {
        self.no_gaps = match self.extent() {
            Some((lo, hi)) => {
                let solid = hi
                    .checked_sub(lo)
                    .and_then(|d| d.checked_add(1))
                    .is_some_and(|n| n == self.chunks.len() as u64);
                if solid && self.chunks.contains_key(&self.playhead) {
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

/// Live scrub blit handles. Evict farthest from playhead (not classic LRU) so
/// densify ahead cannot wipe just-presented frames (Prev stayed cold on long-GOP).
pub struct ScrubBlitLru {
    bitmaps: BTreeMap<FrameId, ImageBitmap>,
    max: usize,
    playhead: FrameId,
}

impl ScrubBlitLru {
    pub fn new(cap: usize) -> Self {
        Self {
            bitmaps: BTreeMap::new(),
            max: cap.max(1),
            playhead: 0,
        }
    }

    /// Resize live blit window (profile policy). Evicts farthest from playhead.
    pub fn set_max(&mut self, cap: usize) {
        self.max = cap.max(1);
        while self.bitmaps.len() > self.max {
            let Some(victim) = self.farthest_from_playhead_excluding(None) else {
                break;
            };
            if let Some(bmp) = self.bitmaps.remove(&victim) {
                bmp.close();
            } else {
                break;
            }
        }
    }

    pub fn clear(&mut self) {
        for (_, bmp) in std::mem::take(&mut self.bitmaps) {
            bmp.close();
        }
    }

    #[allow(dead_code)]
    pub fn invalidate(&mut self, frame: FrameId) {
        if let Some(bmp) = self.bitmaps.remove(&frame) {
            bmp.close();
        }
    }

    pub fn set_playhead(&mut self, playhead: FrameId) {
        self.playhead = playhead;
    }

    /// Frames with a live `ImageBitmap` (demo Exact strip / step-ready).
    pub fn cached_frame_ids(&self) -> Vec<FrameId> {
        self.bitmaps.keys().copied().collect()
    }

    pub fn contains(&self, frame: FrameId) -> bool {
        self.bitmaps.contains_key(&frame)
    }

    /// Closest live Exact blit to `target` (floor/ceil by `|id − target|`).
    pub fn nearest_frame(&self, target: FrameId) -> Option<FrameId> {
        let floor = self.bitmaps.range(..=target).next_back().map(|(id, _)| *id);
        let ceil = self.bitmaps.range(target..).next().map(|(id, _)| *id);
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

    /// Next frame missing a live bitmap near `playhead` (biased by `dir`).
    ///
    /// Used after Play wipes blit while scrub markers stay — densify must see
    /// ImageBitmap holes, not only marker gaps. At capacity with a solid run,
    /// re-arm outside the extent when `playhead` is within
    /// `margin_left`/`margin_right` of an **expandable** edge (same pair as
    /// markers). Outside gaps that would only thrash (evict closer residents)
    /// return `None` — Exact `put` used to always accept and shred the far end
    /// (100% CPU flicker at timeline origin/EOF).
    pub fn next_gap_frame(
        &self,
        playhead: FrameId,
        last: FrameId,
        dir: i8,
        margin_left: u64,
        margin_right: u64,
    ) -> Option<FrameId> {
        let playhead = playhead.min(last);
        if self.bitmaps.is_empty() {
            return Some(playhead);
        }
        if !self.bitmaps.contains_key(&playhead) {
            return Some(playhead);
        }
        let at_cap = self.bitmaps.len() >= self.max;
        let (run_lo, run_hi) = walk_run_bounds(&self.bitmaps, playhead);
        let solid_len = run_hi.saturating_sub(run_lo).saturating_add(1);
        let solid_at_cap =
            at_cap && (solid_len as usize >= self.max || solid_len == self.bitmaps.len() as u64);
        if solid_at_cap {
            let extent = Some((run_lo, run_hi));
            if !scrub_gap_fill_armed(true, playhead, extent, last, margin_left, margin_right) {
                return None;
            }
            let gap = next_gap_outside_extent(playhead, last, extent, dir)?;
            if self.would_evict_closer_than(gap, playhead) {
                return None;
            }
            return Some(gap);
        }
        let no_gaps = match (
            self.bitmaps.keys().next().copied(),
            self.bitmaps.keys().next_back().copied(),
        ) {
            (Some(lo), Some(hi)) => hi
                .saturating_sub(lo)
                .saturating_add(1)
                .eq(&(self.bitmaps.len() as u64)),
            _ => false,
        };
        let extent = self
            .bitmaps
            .keys()
            .next()
            .copied()
            .zip(self.bitmaps.keys().next_back().copied());
        // At capacity a solid map must not expand without arming (same as above).
        if at_cap && no_gaps {
            if !scrub_gap_fill_armed(true, playhead, extent, last, margin_left, margin_right) {
                return None;
            }
            let gap = next_gap_outside_extent(playhead, last, extent, dir)?;
            if self.would_evict_closer_than(gap, playhead) {
                return None;
            }
            return Some(gap);
        }
        let gap = FrameRunIndex::new().search(
            &self.bitmaps,
            playhead,
            playhead,
            last,
            no_gaps,
            extent,
            dir,
        )?;
        if at_cap && self.would_evict_closer_than(gap, playhead) {
            return None;
        }
        Some(gap)
    }

    pub fn get(&mut self, frame: FrameId) -> Option<&ImageBitmap> {
        self.bitmaps.get(&frame)
    }

    /// Insert or replace a live bitmap. Returns `false` when refused at capacity
    /// (would only evict a closer resident — caller must close unused planes).
    pub fn put(&mut self, frame: FrameId, bitmap: ImageBitmap) -> bool {
        use std::collections::btree_map::Entry;
        let replacing = self.bitmaps.contains_key(&frame);
        if !replacing
            && self.bitmaps.len() >= self.max
            && self.would_evict_closer_than(frame, self.playhead)
        {
            bitmap.close();
            return false;
        }
        match self.bitmaps.entry(frame) {
            Entry::Occupied(mut e) => {
                let old = e.insert(bitmap);
                old.close();
                return true;
            }
            Entry::Vacant(v) => {
                v.insert(bitmap);
            }
        }
        while self.bitmaps.len() > self.max {
            let Some(victim) = self.farthest_from_playhead_excluding(Some(frame)) else {
                break;
            };
            if let Some(bmp) = self.bitmaps.remove(&victim) {
                bmp.close();
            } else {
                break;
            }
        }
        if self.bitmaps.len() > self.max {
            if let Some(bmp) = self.bitmaps.remove(&frame) {
                bmp.close();
            }
            return false;
        }
        true
    }

    fn would_evict_closer_than(&self, gap: FrameId, playhead: FrameId) -> bool {
        if self.bitmaps.len() < self.max {
            return false;
        }
        let gap_dist = gap.abs_diff(playhead);
        self.max_dist_from_playhead(playhead)
            .is_some_and(|far| far <= gap_dist)
    }

    fn max_dist_from_playhead(&self, playhead: FrameId) -> Option<u64> {
        let lo = self.bitmaps.keys().copied().find(|&f| f != playhead)?;
        let hi = self
            .bitmaps
            .keys()
            .copied()
            .rev()
            .find(|&f| f != playhead)?;
        Some(lo.abs_diff(playhead).max(hi.abs_diff(playhead)))
    }

    fn farthest_from_playhead_excluding(&self, protect: Option<FrameId>) -> Option<FrameId> {
        let aim = self.playhead;
        self.bitmaps
            .keys()
            .copied()
            .filter(|&f| Some(f) != protect && f != aim)
            .max_by_key(|f| f.abs_diff(aim))
    }
}

impl Drop for ScrubBlitLru {
    fn drop(&mut self) {
        self.clear();
    }
}

/// One GOP of hover / filmstrip thumbs at **caller-requested** bitmap size.
pub struct ThumbGop {
    #[allow(dead_code)] // GOP identity for filmstrip follow-ups
    pub key_frame: FrameId,
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
                    key_frame: key,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn next_gap_prefers_nearest_behind_on_tie() {
        let c = ScrubChunkStore::new(8);
        assert_eq!(c.next_gap_frame(5, 10, 0), Some(5));
    }

    #[test]
    fn next_gap_expands_both_directions() {
        let c = ScrubChunkStore::new(8);
        assert_eq!(c.next_gap_frame(3, 9, 0), Some(3));
    }

    #[test]
    fn blit_next_gap_when_empty_returns_playhead() {
        let blit = ScrubBlitLru::new(8);
        assert_eq!(blit.next_gap_frame(12, 100, 0, 10, 5), Some(12));
    }

    #[test]
    fn refuse_farther_insert_when_playhead_at_origin() {
        assert!(would_evict_closer_than_dist(8, 0, 7));
        assert!(!would_evict_closer_than_dist(12, 5, 5));
    }

    #[test]
    fn blit_would_evict_blocks_far_gap_at_origin() {
        // Cap window [0,7], playhead 0 — outside gap 8 is farther than farthest
        // resident (7) → saturated (no expand thrash).
        assert!(would_evict_closer_than_dist(8, 0, 7));
        // Near-edge slide mid-window: gap just outside is closer than far end.
        assert!(!would_evict_closer_than_dist(96, 90, 90));
    }

    fn would_evict_closer_than_dist(gap: FrameId, playhead: FrameId, far_dist: u64) -> bool {
        far_dist <= gap.abs_diff(playhead)
    }
}
