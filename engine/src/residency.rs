//! Aim-centered frame residency shared by the Exact scrub planes (native-testable).
//!
//! - [`AimWindow`] — capped `FrameId` map; eviction drops the resident farthest
//!   from the aim (not classic LRU) so densify ahead cannot wipe just-presented
//!   frames. Used by Exact blit; Covered markers do **not** share this cap.
//! - [`ScrubChunkStore`] — Covered densify markers (gap geometry / dim strip).
//!   Uncapped for the open clip — presence-only, cheap; Covered-only densify
//!   hard-bounded to aim±fixed margin (idle must not walk the whole file).
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

    /// Evict residents farther from aim than `radius` (inclusive keep).
    ///
    /// Used when the Exact plane is locally idle (no near gap / no `put`) so
    /// farthest-from-aim still runs — otherwise Covered-only densify leaves
    /// orphan bitmaps past a hole on the strip.
    pub fn take_farther_than(&mut self, radius: u64) -> Vec<(FrameId, V)> {
        let mut out = Vec::new();
        while let Some(far) = self.farthest_evictable(None) {
            if far.abs_diff(self.aim) <= radius {
                break;
            }
            let Some(v) = self.items.remove(&far) else {
                break;
            };
            out.push((far, v));
        }
        out
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
    ///
    /// Covered is uncapped (no per-frame evict); kept for Exact/AimWindow helpers
    /// and tests that still shrink capped windows.
    #[allow(dead_code)]
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

/// Covered scrub markers for the open clip (presence-only, uncapped).
///
/// Exact bitmaps stay on a separate capped [`AimWindow`]. Covered-only densify
/// is aim±margin bounded — uncapped store must not mean “fill the whole file
/// on idle”. **no equivalent** (scrub local).
pub struct ScrubChunkStore {
    /// Map + aim helpers only — cap is [`usize::MAX`] (never refuse / evict).
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
    pub fn new() -> Self {
        Self {
            window: AimWindow::new(usize::MAX),
            runs: FrameRunIndex::new(),
            no_gaps: false,
        }
    }

    /// Covered is uncapped — kept for pipeline meters (`0` = no ceiling).
    pub fn max_frames(&self) -> usize {
        0
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
        // Contiguous island: only expand near aim edges (idle must not walk EOF).
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
        self.window.insert(frame, chunk);
        self.runs
            .note_insert(self.window.items(), self.playhead(), frame);
        self.refresh_no_gaps();
        true
    }

    /// Replace-or-insert (presented frame must match canvas).
    pub fn put_chunk(&mut self, frame: FrameId, chunk: ScrubChunk) {
        let fresh = !self.window.contains(frame);
        self.window.insert(frame, chunk);
        if fresh {
            self.runs
                .note_insert(self.window.items(), self.playhead(), frame);
            self.refresh_no_gaps();
        }
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

    /// No marker ceiling — saturated only when there is no gap left to aim.
    pub fn gaps_saturated(&self, playhead: FrameId, last: FrameId, dir: i8) -> bool {
        self.next_gap_frame(playhead, last, dir).is_none()
    }

    pub fn gop_complete(&self, gop_ids: &[FrameId]) -> bool {
        if gop_ids.is_empty() {
            return false;
        }
        gop_ids.iter().all(|id| self.has_frame(*id))
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

impl Default for ScrubChunkStore {
    fn default() -> Self {
        Self::new()
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
        let c = ScrubChunkStore::new();
        assert_eq!(c.next_gap_frame(5, 10, 0), Some(5));
    }

    #[test]
    fn next_gap_expands_both_directions() {
        let c = ScrubChunkStore::new();
        assert_eq!(c.next_gap_frame(3, 9, 0), Some(3));
    }

    #[test]
    fn blit_next_gap_when_empty_returns_playhead() {
        let w: AimWindow<()> = AimWindow::new(8);
        assert_eq!(w.capped_next_gap(12, 100, 0, 10, 5), Some(12));
    }

    #[test]
    fn take_farther_than_drops_orphans_past_hole() {
        // Long Exact island at aim + orphan past a gap — densify Covered-only
        // never Exact-puts, so farthest eviction must still drop the orphan.
        let mut w = window_with(8, 1000, &[995, 996, 997, 998, 999, 1000, 1001, 1400]);
        let dropped: Vec<_> = w
            .take_farther_than(8)
            .into_iter()
            .map(|(id, _)| id)
            .collect();
        assert_eq!(dropped, vec![1400]);
        assert!(w.contains(1000) && w.contains(995));
        assert!(!w.contains(1400));
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
    fn chunk_store_keeps_trail_uncapped() {
        let mut c = ScrubChunkStore::new();
        c.touch_playhead(10);
        for f in [8, 9, 10] {
            assert!(c.insert_chunk(f, ScrubChunk));
        }
        assert!(c.insert_chunk(11, ScrubChunk));
        // Far from aim still lands — Covered is clip-lifetime presence.
        assert!(c.insert_chunk(20, ScrubChunk));
        assert_eq!(c.cached_frame_ids(), vec![8, 9, 10, 11, 20]);
        assert_eq!(c.max_frames(), 0);
        assert!(!c.gaps_saturated(10, 30, 0));
    }

    #[test]
    fn put_chunk_always_lands() {
        let mut c = ScrubChunkStore::new();
        c.touch_playhead(0);
        assert!(c.insert_chunk(0, ScrubChunk));
        assert!(c.insert_chunk(1, ScrubChunk));
        c.put_chunk(50, ScrubChunk);
        assert!(c.has_frame(0) && c.has_frame(1) && c.has_frame(50));
        assert_eq!(c.frame_count(), 3);
    }

    #[test]
    fn gap_fill_armed_idle_needs_edge_when_solid() {
        let mut c = ScrubChunkStore::new();
        c.touch_playhead(50);
        for f in 40..=60 {
            assert!(c.insert_chunk(f, ScrubChunk));
        }
        assert!(c.no_gaps());
        // Mid-island + solid → only edge margins re-arm (idle must not walk EOF).
        assert!(!c.gap_fill_armed(50, 1000, 8, 8));
        assert!(c.gap_fill_armed(45, 1000, 8, 8));
    }
}
