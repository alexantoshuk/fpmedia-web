//! Aim-centered frame residency shared by the Exact scrub planes (native-testable).
//!
//! - [`AimWindow`] — capped `FrameId` map; eviction drops the resident farthest
//!   from the aim (not classic LRU) so densify ahead cannot wipe just-presented
//!   frames.
//! - [`ScrubChunkStore`] — densify resident markers (gap geometry / Exact strip).
//! - [`FrameRunIndex`] — contiguous-run geometry + nearest-gap search.
//!
//! The live blit plane (`decode::frame_cache::ScrubBlitLru`) wraps an
//! `AimWindow<ImageBitmap>`. Local invention — mediabunny has no densify store.

// Callers live under `decode` / `video_player` (`wasm32` + `webcodecs` only).
// Native builds still compile this module for unit tests.
#![cfg_attr(
    not(all(target_arch = "wasm32", feature = "webcodecs")),
    allow(dead_code)
)]

use std::cell::Cell;
use std::collections::BTreeMap;
use std::ops::Bound;

use crate::policy::{nearer_gap_with_dir, scrub_gap_fill_armed};
use crate::types::FrameId;

/// Resident densify marker in [`ScrubChunkStore`].
///
/// Presence-only — canvas present uses the live blit plane.
#[derive(Clone, Copy, Debug, Default)]
pub struct ScrubChunk;

/// Capped `FrameId → V` map evicting the resident farthest from `aim`.
pub struct AimWindow<V> {
    items: BTreeMap<FrameId, V>,
    max: usize,
    aim: FrameId,
}

impl<V> AimWindow<V> {
    pub fn new(max: usize) -> Self {
        Self {
            items: BTreeMap::new(),
            max: max.max(1),
            aim: 0,
        }
    }

    pub fn items(&self) -> &BTreeMap<FrameId, V> {
        &self.items
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn max(&self) -> usize {
        self.max
    }

    /// Set the cap only — callers shrink via [`Self::take_farthest`].
    pub fn set_max(&mut self, max: usize) {
        self.max = max.max(1);
    }

    pub fn aim(&self) -> FrameId {
        self.aim
    }

    pub fn set_aim(&mut self, aim: FrameId) {
        self.aim = aim;
    }

    pub fn contains(&self, frame: FrameId) -> bool {
        self.items.contains_key(&frame)
    }

    pub fn get(&self, frame: FrameId) -> Option<&V> {
        self.items.get(&frame)
    }

    pub fn keys(&self) -> impl DoubleEndedIterator<Item = FrameId> + '_ {
        self.items.keys().copied()
    }

    /// Raw insert (no cap check). Returns the replaced value.
    pub fn insert(&mut self, frame: FrameId, value: V) -> Option<V> {
        self.items.insert(frame, value)
    }

    pub fn remove(&mut self, frame: FrameId) -> Option<V> {
        self.items.remove(&frame)
    }

    /// Remove everything; caller releases the values.
    pub fn take_all(&mut self) -> BTreeMap<FrameId, V> {
        std::mem::take(&mut self.items)
    }

    pub fn extent(&self) -> Option<(FrameId, FrameId)> {
        let lo = *self.items.keys().next()?;
        let hi = *self.items.keys().next_back()?;
        Some((lo, hi))
    }

    /// Closest resident to `target` (floor / ceil by `|id − target|`, tie → floor).
    pub fn nearest(&self, target: FrameId) -> Option<FrameId> {
        let floor = self.items.range(..=target).next_back().map(|(id, _)| *id);
        let ceil = self.items.range(target..).next().map(|(id, _)| *id);
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

    /// At cap, inserting `gap` would only evict a resident at least as close to `from`.
    pub fn would_evict_closer_than(&self, gap: FrameId, from: FrameId) -> bool {
        if self.items.len() < self.max {
            return false;
        }
        let gap_dist = gap.abs_diff(from);
        self.max_dist_from(from).is_some_and(|far| far <= gap_dist)
    }

    fn max_dist_from(&self, from: FrameId) -> Option<u64> {
        let lo = self.keys().find(|&f| f != from)?;
        let hi = self.keys().rev().find(|&f| f != from)?;
        Some(lo.abs_diff(from).max(hi.abs_diff(from)))
    }

    /// Farthest resident from `aim`, never `aim` or `protect` (tie → higher id).
    pub fn farthest_evictable(&self, protect: Option<FrameId>) -> Option<FrameId> {
        let allowed = |f: FrameId| Some(f) != protect && f != self.aim;
        let lo = self.keys().find(|&f| allowed(f))?;
        let hi = self.keys().rev().find(|&f| allowed(f))?;
        [lo, hi].into_iter().max_by_key(|f| f.abs_diff(self.aim))
    }

    /// Evict [`Self::farthest_evictable`]; caller releases the value.
    pub fn take_farthest(&mut self, protect: Option<FrameId>) -> Option<(FrameId, V)> {
        let victim = self.farthest_evictable(protect)?;
        self.items.remove(&victim).map(|v| (victim, v))
    }

    /// Next frame missing near `playhead` (biased by `dir`) for a plane with no
    /// persistent run cache. At capacity with a solid run, re-arm outside the
    /// extent only when `playhead` is within `margin_left` / `margin_right` of an
    /// expandable edge; gaps that would only evict closer residents return `None`
    /// (accepting them shredded the far end — 100% CPU flicker at origin / EOF).
    pub fn capped_next_gap(
        &self,
        playhead: FrameId,
        last: FrameId,
        dir: i8,
        margin_left: u64,
        margin_right: u64,
    ) -> Option<FrameId> {
        let playhead = playhead.min(last);
        if self.items.is_empty() {
            return Some(playhead);
        }
        if !self.items.contains_key(&playhead) {
            return Some(playhead);
        }
        let at_cap = self.items.len() >= self.max;
        let (run_lo, run_hi) = walk_run_bounds(&self.items, playhead);
        let solid_len = run_hi.saturating_sub(run_lo).saturating_add(1);
        let solid_at_cap =
            at_cap && (solid_len as usize >= self.max || solid_len == self.items.len() as u64);
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
        let extent = self.extent();
        let no_gaps = extent.is_some_and(|(lo, hi)| {
            hi.saturating_sub(lo)
                .saturating_add(1)
                .eq(&(self.items.len() as u64))
        });
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
            &self.items,
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
}

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
    window: AimWindow<ScrubChunk>,
    runs: FrameRunIndex,
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
            window: AimWindow::new(max_frames),
            runs: FrameRunIndex::new(),
            no_gaps: false,
        }
    }

    pub fn set_max_frames(&mut self, max_frames: usize) {
        let max_frames = max_frames.max(1);
        if max_frames == self.window.max() {
            return;
        }
        self.window.set_max(max_frames);
        self.enforce_frame_budget(None);
    }

    pub fn max_frames(&self) -> usize {
        self.window.max()
    }

    pub fn clear(&mut self) {
        self.window.take_all();
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
        if self.frame_count() < self.max_frames() {
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
        for id in self.window.keys() {
            match prev {
                Some(p) if id == p + 1 => {}
                _ => runs += 1,
            }
            prev = Some(id);
        }
        runs
    }

    pub fn frame_count(&self) -> usize {
        self.window.len()
    }

    /// Marker ids (gap geometry). Demo strip dim `Covered` ticks use these.
    pub fn cached_frame_ids(&self) -> Vec<FrameId> {
        self.window.keys().collect()
    }

    pub fn extent(&self) -> Option<(FrameId, FrameId)> {
        self.window.extent()
    }

    pub fn touch_playhead(&mut self, frame: FrameId) {
        if frame != self.window.aim() {
            self.runs.on_aim_change(frame);
        }
        self.window.set_aim(frame);
        self.enforce_frame_budget(Some(frame));
    }

    pub fn playhead(&self) -> FrameId {
        self.window.aim()
    }

    /// Nearest Covered marker (strip / gap geometry). Soft-drag preview must use
    /// Exact [`crate::decode::frame_cache::ScrubBlitLru::nearest_frame`] instead.
    #[allow(dead_code)] // kept for diagnostics / future marker-aim helpers
    pub fn nearest_frame(&self, target: FrameId) -> Option<FrameId> {
        self.window.nearest(target)
    }

    pub fn has_frame(&self, frame: FrameId) -> bool {
        self.window.contains(frame)
    }

    /// Insert without replacing. Returns `true` when newly resident.
    pub fn insert_chunk(&mut self, frame: FrameId, chunk: ScrubChunk) -> bool {
        if self.window.contains(frame) {
            return false;
        }
        if self.frame_count() >= self.max_frames()
            && self.window.would_evict_closer_than(frame, self.playhead())
        {
            return false;
        }
        while self.frame_count() >= self.max_frames() {
            if !self.evict_one(Some(frame)) {
                break;
            }
        }
        if self.frame_count() >= self.max_frames() {
            return false;
        }
        self.window.insert(frame, chunk);
        self.runs
            .note_insert(self.window.items(), self.playhead(), frame);
        self.refresh_no_gaps();
        true
    }

    /// Replace-or-insert (presented frame must match canvas).
    pub fn put_chunk(&mut self, frame: FrameId, chunk: ScrubChunk) {
        if self.window.contains(frame) {
            self.window.insert(frame, chunk);
            return;
        }
        while self.frame_count() >= self.max_frames() {
            if self.evict_one(Some(frame)) {
                continue;
            }
            let playhead = self.playhead();
            if playhead != frame && self.remove_frame(playhead) {
                continue;
            }
            let other = self.window.keys().find(|&f| f != frame);
            if let Some(victim) = other {
                self.remove_frame(victim);
                continue;
            }
            break;
        }
        if self.frame_count() >= self.max_frames() {
            return;
        }
        self.window.insert(frame, chunk);
        self.runs
            .note_insert(self.window.items(), self.playhead(), frame);
        self.refresh_no_gaps();
    }

    pub fn next_gap_frame(&self, playhead: FrameId, last: FrameId, dir: i8) -> Option<FrameId> {
        self.runs.search(
            self.window.items(),
            self.playhead(),
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
            || self.frame_count() >= self.max_frames()
                && gap.is_some_and(|g| self.window.would_evict_closer_than(g, playhead))
    }

    pub fn gop_complete(&self, gop_ids: &[FrameId]) -> bool {
        if gop_ids.is_empty() {
            return false;
        }
        gop_ids.iter().all(|id| self.has_frame(*id))
    }

    fn enforce_frame_budget(&mut self, protect: Option<FrameId>) {
        while self.frame_count() > self.max_frames() {
            if !self.evict_one(protect) {
                break;
            }
        }
    }

    fn evict_one(&mut self, protect: Option<FrameId>) -> bool {
        let Some(victim) = self.window.farthest_evictable(protect) else {
            return false;
        };
        self.remove_frame(victim)
    }

    fn remove_frame(&mut self, frame: FrameId) -> bool {
        if self.window.remove(frame).is_none() {
            return false;
        }
        self.runs.note_remove(self.playhead(), frame);
        self.refresh_no_gaps();
        true
    }

    fn refresh_no_gaps(&mut self) {
        self.no_gaps = match self.extent() {
            Some((lo, hi)) => {
                let solid = hi
                    .checked_sub(lo)
                    .and_then(|d| d.checked_add(1))
                    .is_some_and(|n| n == self.window.len() as u64);
                if solid && self.window.contains(self.playhead()) {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn window_with(max: usize, aim: FrameId, ids: &[FrameId]) -> AimWindow<()> {
        let mut w = AimWindow::new(max);
        w.set_aim(aim);
        for &id in ids {
            w.insert(id, ());
        }
        w
    }

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
        let w: AimWindow<()> = AimWindow::new(8);
        assert_eq!(w.capped_next_gap(12, 100, 0, 10, 5), Some(12));
    }

    #[test]
    fn refuse_farther_insert_when_playhead_at_origin() {
        // Cap window [0,7], aim 0 — outside gap 8 is farther than the far end (7).
        let w = window_with(8, 0, &[0, 1, 2, 3, 4, 5, 6, 7]);
        assert!(w.would_evict_closer_than(8, 0));
        // Mid-window slide: gap just outside is closer than the far end.
        let w = window_with(8, 5, &[0, 1, 2, 3, 4, 5, 6, 7]);
        assert!(!w.would_evict_closer_than(8, 5));
        // Below cap never refuses.
        let w = window_with(9, 0, &[0, 1, 2, 3, 4, 5, 6, 7]);
        assert!(!w.would_evict_closer_than(100, 0));
    }

    #[test]
    fn nearest_tie_prefers_floor() {
        let w = window_with(8, 0, &[10, 20]);
        assert_eq!(w.nearest(15), Some(10));
        assert_eq!(w.nearest(16), Some(20));
        assert_eq!(w.nearest(5), Some(10));
        assert_eq!(w.nearest(25), Some(20));
    }

    #[test]
    fn farthest_skips_aim_and_protect_tie_takes_higher() {
        let w = window_with(8, 10, &[5, 10, 15]);
        assert_eq!(w.farthest_evictable(None), Some(15));
        assert_eq!(w.farthest_evictable(Some(15)), Some(5));
        let w = window_with(8, 10, &[10]);
        assert_eq!(w.farthest_evictable(None), None);
    }

    #[test]
    fn chunk_store_evicts_farthest_from_playhead() {
        let mut c = ScrubChunkStore::new(3);
        c.touch_playhead(10);
        for f in [8, 9, 10] {
            assert!(c.insert_chunk(f, ScrubChunk));
        }
        // 11 is closer than the far end (8) → evicts 8.
        assert!(c.insert_chunk(11, ScrubChunk));
        assert_eq!(c.cached_frame_ids(), vec![9, 10, 11]);
        // 20 would only evict closer residents → refused.
        assert!(!c.insert_chunk(20, ScrubChunk));
        assert_eq!(c.cached_frame_ids(), vec![9, 10, 11]);
        assert!(c.no_gaps());
    }

    #[test]
    fn put_chunk_forces_presented_frame_in() {
        let mut c = ScrubChunkStore::new(2);
        c.touch_playhead(0);
        assert!(c.insert_chunk(0, ScrubChunk));
        assert!(c.insert_chunk(1, ScrubChunk));
        // Far presented frame still lands (evicts 1, then playhead 0 if needed).
        c.put_chunk(50, ScrubChunk);
        assert!(c.has_frame(50));
        assert_eq!(c.frame_count(), 2);
    }

    #[test]
    fn shrinking_cap_keeps_playhead() {
        let mut c = ScrubChunkStore::new(5);
        c.touch_playhead(2);
        for f in 0..5 {
            assert!(c.insert_chunk(f, ScrubChunk));
        }
        c.set_max_frames(1);
        assert_eq!(c.cached_frame_ids(), vec![2]);
    }
}
