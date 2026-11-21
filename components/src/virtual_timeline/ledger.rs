//! Timeline residency helpers (global FrameId) — Covered + Fetch + thumb markers sole on VT + parked Exact AimWindow.
//!
//! Shipped Exact = slot `ScrubBlitLru` (PresentHit = slot blit). Covered + Fetch + Pause-thumb
//! markers live here (global FrameIds; survive clip leave). Thumb **bitmaps** =
//! [`super::VtThumbCache`] (feature `thumbs`) — not slot ThumbCache / not Exact ledger write.
//! Design: `.ai/architecture.md` § Virtual Timeline.
//! **no equivalent** (scrub local).

use std::collections::BTreeSet;

use crate::frame_axis::GlobalFrame;
use fpmedia_web::{AimWindow, FrameId, FrameRun, MIN_SCRUB_BLIT_FRAMES, merge_frame_runs};

/// Outcome of [`ExactLedger::put_exact`] — caller releases bitmaps on wasm.
#[derive(Debug)]
pub struct PutExactOutcome<V> {
    /// `false` when refused at capacity (would only evict a closer resident).
    pub accepted: bool,
    pub replaced: Option<V>,
    pub evicted: Vec<(GlobalFrame, V)>,
    /// Set when `accepted == false` and the new value never entered the window.
    pub refused: Option<V>,
}

/// Global Exact + Covered + Fetch + thumb-marker ledger for one open timeline session.
pub struct ExactLedger<V> {
    exact: AimWindow<V>,
    /// Uncapped Covered markers (gap / dim strip) in **global** FrameIds.
    covered: BTreeSet<FrameId>,
    /// Inclusive playable Fetch runs in **global** FrameIds (interval-merged; dense OK).
    fetch: Vec<FrameRun>,
    /// Pause progressive thumb keys filled at least once this session (global FrameIds).
    /// Bitmaps = VT `VtThumbCache`; markers are optional trail / strip hints.
    thumbs: BTreeSet<FrameId>,
}

impl<V> ExactLedger<V> {
    /// Store keys are the global axis by construction; tag them at the API edge.
    fn globalize(pairs: impl IntoIterator<Item = (FrameId, V)>) -> Vec<(GlobalFrame, V)> {
        pairs
            .into_iter()
            .map(|(k, v)| (GlobalFrame::new(k), v))
            .collect()
    }

    /// Cap = [`MIN_SCRUB_BLIT_FRAMES`] floor (same order as single-clip Exact).
    /// Hosts should call [`Self::set_capacity`] from real `scrub_blit_frame_budget`.
    pub fn new() -> Self {
        Self::with_capacity(MIN_SCRUB_BLIT_FRAMES)
    }

    pub fn with_capacity(cap: usize) -> Self {
        Self {
            exact: AimWindow::new(cap),
            covered: BTreeSet::new(),
            fetch: Vec::new(),
            thumbs: BTreeSet::new(),
        }
    }

    pub fn capacity(&self) -> usize {
        self.exact.max()
    }

    pub fn exact_len(&self) -> usize {
        self.exact.len()
    }

    pub fn aim(&self) -> GlobalFrame {
        GlobalFrame::new(self.exact.aim())
    }

    /// Global aim for farthest-from-aim Exact eviction.
    pub fn set_aim(&mut self, global: GlobalFrame) {
        self.exact.set_aim(global.get());
    }

    /// Drop Exact farther from aim than capacity radius (engine blit parity).
    /// Caller releases values. **no equivalent** (scrub local).
    pub fn take_orphans_beyond_aim(&mut self) -> Vec<(GlobalFrame, V)> {
        let radius = self.exact.max() as FrameId;
        Self::globalize(self.exact.take_farther_than(radius))
    }

    /// Resize Exact cap; returns victims over the new max (caller releases).
    pub fn set_capacity(&mut self, cap: usize) -> Vec<(GlobalFrame, V)> {
        self.exact.set_max(cap);
        let mut out = Vec::new();
        while self.exact.len() > self.exact.max() {
            let Some(pair) = self.exact.take_farthest(None) else {
                break;
            };
            out.push(pair);
        }
        Self::globalize(out)
    }

    pub fn has_exact(&self, global: GlobalFrame) -> bool {
        self.exact.contains(global.get())
    }

    pub fn get_exact(&self, global: GlobalFrame) -> Option<&V> {
        self.exact.get(global.get())
    }

    /// Insert or replace Exact at `global`. Same refuse gate as engine scrub blit.
    pub fn put_exact(&mut self, global: GlobalFrame, value: V) -> PutExactOutcome<V> {
        let key = global.get();
        if !self.exact.contains(key)
            && self.exact.len() >= self.exact.max()
            && self.exact.would_evict_closer_than(key, self.exact.aim())
        {
            return PutExactOutcome {
                accepted: false,
                replaced: None,
                evicted: Vec::new(),
                refused: Some(value),
            };
        }
        let replaced = self.exact.insert(key, value);
        let mut evicted = Vec::new();
        while self.exact.len() > self.exact.max() {
            let Some(pair) = self.exact.take_farthest(Some(key)) else {
                break;
            };
            evicted.push(pair);
        }
        if self.exact.len() > self.exact.max() {
            if let Some(v) = self.exact.remove(key) {
                evicted.push((key, v));
            }
            return PutExactOutcome {
                accepted: false,
                replaced,
                evicted: Self::globalize(evicted),
                refused: None,
            };
        }
        PutExactOutcome {
            accepted: true,
            replaced,
            evicted: Self::globalize(evicted),
            refused: None,
        }
    }

    pub fn remove_exact(&mut self, global: GlobalFrame) -> Option<V> {
        self.exact.remove(global.get())
    }

    pub fn note_covered(&mut self, global: GlobalFrame) {
        self.covered.insert(global.get());
    }

    pub fn note_covered_run(&mut self, from: GlobalFrame, to_inclusive: GlobalFrame) {
        let (lo, hi) = if from <= to_inclusive {
            (from.get(), to_inclusive.get())
        } else {
            (to_inclusive.get(), from.get())
        };
        for id in lo..=hi {
            self.covered.insert(id);
        }
    }

    pub fn has_covered(&self, global: GlobalFrame) -> bool {
        self.covered.contains(&global.get())
    }

    pub fn exact_frame_ids(&self) -> Vec<FrameId> {
        self.exact.keys().collect()
    }

    pub fn covered_frame_ids(&self) -> Vec<FrameId> {
        self.covered.iter().copied().collect()
    }

    /// Exact strip runs in **global** FrameIds (`ReadyFrameMark` / TimelineMeta).
    pub fn exact_ready_marks(&self) -> Vec<FrameRun> {
        fpmedia_web::coalesce_frame_runs(self.exact.keys())
    }

    /// Covered strip runs in **global** FrameIds.
    pub fn covered_ready_marks(&self) -> Vec<FrameRun> {
        fpmedia_web::coalesce_frame_runs(self.covered.iter().copied())
    }

    fn fetch_covers_raw(&self, lo: FrameId, hi: FrameId) -> bool {
        self.fetch.iter().any(|r| r.lo <= lo && hi <= r.hi)
    }

    /// Merge an inclusive playable run. Returns `true` if the union grew.
    /// Interval store (not per-frame) — playable spans are often dense. **no equivalent**.
    pub fn note_fetch_run(&mut self, from: GlobalFrame, to_inclusive: GlobalFrame) -> bool {
        let (lo, hi) = if from <= to_inclusive {
            (from.get(), to_inclusive.get())
        } else {
            (to_inclusive.get(), from.get())
        };
        if self.fetch_covers_raw(lo, hi) {
            return false;
        }
        self.fetch.push(FrameRun { lo, hi });
        self.fetch = merge_frame_runs(std::mem::take(&mut self.fetch));
        true
    }

    /// `true` when `[lo, hi]` is already ⊆ some resident Fetch run.
    pub fn fetch_covers(&self, lo: GlobalFrame, hi: GlobalFrame) -> bool {
        self.fetch_covers_raw(lo.get(), hi.get())
    }

    /// Fetch strip runs in **global** FrameIds (already coalesced).
    pub fn fetch_ready_marks(&self) -> Vec<FrameRun> {
        self.fetch.clone()
    }

    /// Record a Pause progressive thumb key (global). Returns `true` if new.
    pub fn note_thumb(&mut self, global: GlobalFrame) -> bool {
        self.thumbs.insert(global.get())
    }

    pub fn has_thumb(&self, global: GlobalFrame) -> bool {
        self.thumbs.contains(&global.get())
    }

    pub fn thumb_frame_ids(&self) -> Vec<FrameId> {
        self.thumbs.iter().copied().collect()
    }

    /// Drop all Exact values (caller releases) + clear Covered + Fetch + thumbs.
    pub fn take_all_exact(&mut self) -> Vec<(GlobalFrame, V)> {
        self.covered.clear();
        self.fetch.clear();
        self.thumbs.clear();
        Self::globalize(self.exact.take_all())
    }

    pub fn clear_covered(&mut self) {
        self.covered.clear();
    }

    /// Session wipe only (close·open / media replace) — not Scrub Off / not idle park.
    pub fn clear_fetch(&mut self) {
        self.fetch.clear();
    }

    /// Session wipe only (close·open / media replace) — not idle park / not Scrub Off.
    pub fn clear_thumbs(&mut self) {
        self.thumbs.clear();
    }
}

impl<V> Default for ExactLedger<V> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_cap_not_dual_slot() {
        let mut led = ExactLedger::<u32>::with_capacity(4);
        led.set_aim(GlobalFrame::new(10));
        for g in 8u32..=11 {
            let out = led.put_exact(GlobalFrame::new(g), g);
            assert!(out.accepted, "insert {g}");
            assert!(out.replaced.is_none());
            assert!(out.evicted.is_empty());
            assert!(out.refused.is_none());
        }
        assert_eq!(led.exact_len(), 4);
        // At cap, far from aim refused or evicts far end — still max 4.
        let out = led.put_exact(GlobalFrame::new(100), 100);
        if out.accepted {
            assert!(led.exact_len() <= 4);
        }
        assert!(led.exact_len() <= led.capacity());
    }

    #[test]
    fn hit_miss_and_aim_evict() {
        let mut led = ExactLedger::<u32>::with_capacity(3);
        led.set_aim(GlobalFrame::new(5));
        assert!(!led.has_exact(GlobalFrame::new(5)));
        assert!(led.put_exact(GlobalFrame::new(5), 50).accepted);
        assert!(led.has_exact(GlobalFrame::new(5)));
        assert_eq!(led.get_exact(GlobalFrame::new(5)), Some(&50));

        assert!(led.put_exact(GlobalFrame::new(4), 40).accepted);
        assert!(led.put_exact(GlobalFrame::new(6), 60).accepted);
        assert_eq!(led.exact_len(), 3);

        // Far insert while only near-aim residents → refuse (same gate as scrub blit).
        let out = led.put_exact(GlobalFrame::new(20), 200);
        assert!(!out.accepted);
        assert!(out.refused.is_some());
        assert!(!led.has_exact(GlobalFrame::new(20)));
        assert_eq!(led.exact_len(), 3);

        // Spread residents — insert near aim can evict the far end.
        let mut led = ExactLedger::<u32>::with_capacity(3);
        led.set_aim(GlobalFrame::new(50));
        assert!(led.put_exact(GlobalFrame::new(0), 0).accepted);
        assert!(led.put_exact(GlobalFrame::new(50), 50).accepted);
        assert!(led.put_exact(GlobalFrame::new(100), 100).accepted);
        let out = led.put_exact(GlobalFrame::new(51), 51);
        assert!(out.accepted);
        assert!(led.has_exact(GlobalFrame::new(51)));
        assert_eq!(led.exact_len(), 3);
        assert!(!out.evicted.is_empty());
        assert!(!led.has_exact(GlobalFrame::new(0)) || !led.has_exact(GlobalFrame::new(100)));
    }

    #[test]
    fn covered_uncapped_across_joins() {
        let mut led = ExactLedger::<u32>::new();
        led.note_covered_run(GlobalFrame::new(0), GlobalFrame::new(9)); // clip A
        led.note_covered_run(GlobalFrame::new(10), GlobalFrame::new(19)); // clip B (global join) — survives “leave” A
        assert!(led.has_covered(GlobalFrame::new(0)));
        assert!(led.has_covered(GlobalFrame::new(9)));
        assert!(led.has_covered(GlobalFrame::new(10)));
        assert!(led.has_covered(GlobalFrame::new(19)));
        assert_eq!(led.covered_frame_ids().len(), 20);
        assert_eq!(led.covered_ready_marks(), vec![FrameRun::new(0, 19)]);
        // Idle park must not clear — only session wipe (Scrub Off / close·open).
        led.clear_covered();
        assert!(led.covered_frame_ids().is_empty());
    }

    #[test]
    fn fetch_uncapped_across_joins_interval_merge() {
        let mut led = ExactLedger::<u32>::new();
        assert!(led.note_fetch_run(GlobalFrame::new(0), GlobalFrame::new(9))); // clip A
        assert!(led.note_fetch_run(GlobalFrame::new(10), GlobalFrame::new(19))); // clip B abut → merge
        assert_eq!(led.fetch_ready_marks(), vec![FrameRun::new(0, 19)]);
        assert!(!led.note_fetch_run(GlobalFrame::new(3), GlobalFrame::new(12))); // already covered
        assert!(led.note_fetch_run(GlobalFrame::new(30), GlobalFrame::new(35)));
        assert_eq!(
            led.fetch_ready_marks(),
            vec![FrameRun::new(0, 19), FrameRun::new(30, 35)]
        );
        // Scrub Off clears Covered only — Fetch survives until session wipe.
        led.clear_covered();
        assert_eq!(
            led.fetch_ready_marks(),
            vec![FrameRun::new(0, 19), FrameRun::new(30, 35)]
        );
        led.clear_fetch();
        assert!(led.fetch_ready_marks().is_empty());
    }

    #[test]
    fn thumb_markers_survive_clip_leave() {
        let mut led = ExactLedger::<u32>::new();
        assert!(led.note_thumb(GlobalFrame::new(0)));
        assert!(led.note_thumb(GlobalFrame::new(100))); // clip B global
        assert!(!led.note_thumb(GlobalFrame::new(0)));
        assert!(led.has_thumb(GlobalFrame::new(0)));
        assert!(led.has_thumb(GlobalFrame::new(100)));
        assert_eq!(led.thumb_frame_ids(), vec![0, 100]);
        // Idle rebind must not clear — only session wipe.
        led.clear_covered();
        assert!(led.has_thumb(GlobalFrame::new(0)));
        led.clear_thumbs();
        assert!(led.thumb_frame_ids().is_empty());
    }

    #[test]
    fn default_cap_is_single_clip_floor() {
        let led = ExactLedger::<()>::new();
        assert_eq!(led.capacity(), MIN_SCRUB_BLIT_FRAMES);
    }

    #[test]
    fn exact_ready_marks_coalesce_global_runs() {
        let mut led = ExactLedger::<u32>::with_capacity(16);
        led.set_aim(GlobalFrame::new(5));
        for g in [1u32, 2, 3, 10, 11] {
            assert!(led.put_exact(GlobalFrame::new(g), g).accepted);
        }
        let runs = led.exact_ready_marks();
        assert_eq!(runs, vec![FrameRun::new(1, 3), FrameRun::new(10, 11)]);
    }

    /// Densify bulk sync must keep playhead aim fixed while inserting sorted ids.
    #[test]
    fn bulk_put_keeps_playhead_aim() {
        let mut led = ExactLedger::<u32>::with_capacity(8);
        led.set_aim(GlobalFrame::new(50));
        for g in 40u32..=47 {
            assert!(led.put_exact(GlobalFrame::new(g), g).accepted, "insert {g}");
            assert_eq!(
                led.aim(),
                GlobalFrame::new(50),
                "aim must not walk with insert id"
            );
        }
        assert!(led.has_exact(GlobalFrame::new(40)));
        assert!(led.has_exact(GlobalFrame::new(47)));
        assert_eq!(led.exact_len(), 8);
    }

    #[test]
    fn orphans_beyond_aim_drop_far_exact() {
        let mut led = ExactLedger::with_capacity(4);
        led.set_aim(GlobalFrame::new(0));
        for i in 0..4u32 {
            assert!(led.put_exact(GlobalFrame::new(i), i).accepted);
        }
        led.set_aim(GlobalFrame::new(100));
        let victims = led.take_orphans_beyond_aim();
        assert_eq!(victims.len(), 4);
        assert_eq!(led.exact_len(), 0);
    }
}
