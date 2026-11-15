//! Sole Exact / Covered residency on the virtual timeline axis (global FrameId).
//!
//! One [`AimWindow`] — never 2× dual-slot. Slots decode into this ledger; they do
//! not own Exact policy. PresentHit = [`Self::has_exact`].
//! Design: `.ai/architecture.md` § Virtual Timeline.
//! **no equivalent** (scrub local).

use std::collections::BTreeSet;

use fpmedia_web::{AimWindow, FrameId, MIN_SCRUB_BLIT_FRAMES};

/// Outcome of [`ExactLedger::put_exact`] — caller releases bitmaps on wasm.
#[derive(Debug)]
pub struct PutExactOutcome<V> {
    /// `false` when refused at capacity (would only evict a closer resident).
    pub accepted: bool,
    pub replaced: Option<V>,
    pub evicted: Vec<(FrameId, V)>,
    /// Set when `accepted == false` and the new value never entered the window.
    pub refused: Option<V>,
}

/// Global Exact + Covered ledger for one open timeline session (Single or Playlist).
pub struct ExactLedger<V> {
    exact: AimWindow<V>,
    /// Uncapped Covered markers (gap / dim strip) in **global** FrameIds.
    covered: BTreeSet<FrameId>,
}

impl<V> ExactLedger<V> {
    /// Cap = [`MIN_SCRUB_BLIT_FRAMES`] floor (same order as single-clip Exact).
    /// Hosts should call [`Self::set_capacity`] from real `scrub_blit_frame_budget`.
    pub fn new() -> Self {
        Self::with_capacity(MIN_SCRUB_BLIT_FRAMES)
    }

    pub fn with_capacity(cap: usize) -> Self {
        Self {
            exact: AimWindow::new(cap),
            covered: BTreeSet::new(),
        }
    }

    pub fn capacity(&self) -> usize {
        self.exact.max()
    }

    pub fn exact_len(&self) -> usize {
        self.exact.len()
    }

    pub fn aim(&self) -> FrameId {
        self.exact.aim()
    }

    /// Global aim for farthest-from-aim Exact eviction.
    pub fn set_aim(&mut self, global: FrameId) {
        self.exact.set_aim(global);
    }

    /// Drop Exact farther from aim than capacity radius (engine blit parity).
    /// Caller releases values. **no equivalent** (scrub local).
    pub fn take_orphans_beyond_aim(&mut self) -> Vec<(FrameId, V)> {
        let radius = self.exact.max() as u64;
        self.exact.take_farther_than(radius)
    }

    /// Resize Exact cap; returns victims over the new max (caller releases).
    pub fn set_capacity(&mut self, cap: usize) -> Vec<(FrameId, V)> {
        self.exact.set_max(cap);
        let mut out = Vec::new();
        while self.exact.len() > self.exact.max() {
            let Some(pair) = self.exact.take_farthest(None) else {
                break;
            };
            out.push(pair);
        }
        out
    }

    pub fn has_exact(&self, global: FrameId) -> bool {
        self.exact.contains(global)
    }

    pub fn get_exact(&self, global: FrameId) -> Option<&V> {
        self.exact.get(global)
    }

    /// Insert or replace Exact at `global`. Same refuse gate as engine scrub blit.
    pub fn put_exact(&mut self, global: FrameId, value: V) -> PutExactOutcome<V> {
        if !self.exact.contains(global)
            && self.exact.len() >= self.exact.max()
            && self.exact.would_evict_closer_than(global, self.exact.aim())
        {
            return PutExactOutcome {
                accepted: false,
                replaced: None,
                evicted: Vec::new(),
                refused: Some(value),
            };
        }
        let replaced = self.exact.insert(global, value);
        let mut evicted = Vec::new();
        while self.exact.len() > self.exact.max() {
            let Some(pair) = self.exact.take_farthest(Some(global)) else {
                break;
            };
            evicted.push(pair);
        }
        if self.exact.len() > self.exact.max() {
            if let Some(v) = self.exact.remove(global) {
                evicted.push((global, v));
            }
            return PutExactOutcome {
                accepted: false,
                replaced,
                evicted,
                refused: None,
            };
        }
        PutExactOutcome {
            accepted: true,
            replaced,
            evicted,
            refused: None,
        }
    }

    pub fn remove_exact(&mut self, global: FrameId) -> Option<V> {
        self.exact.remove(global)
    }

    pub fn note_covered(&mut self, global: FrameId) {
        self.covered.insert(global);
    }

    pub fn note_covered_run(&mut self, from: FrameId, to_inclusive: FrameId) {
        let (lo, hi) = if from <= to_inclusive {
            (from, to_inclusive)
        } else {
            (to_inclusive, from)
        };
        for id in lo..=hi {
            self.covered.insert(id);
        }
    }

    pub fn has_covered(&self, global: FrameId) -> bool {
        self.covered.contains(&global)
    }

    pub fn exact_frame_ids(&self) -> Vec<FrameId> {
        self.exact.keys().collect()
    }

    pub fn covered_frame_ids(&self) -> Vec<FrameId> {
        self.covered.iter().copied().collect()
    }

    /// Exact strip runs in **global** FrameIds (`ReadyFrameMark` / TimelineMeta).
    pub fn exact_ready_marks(&self) -> Vec<(FrameId, FrameId)> {
        fpmedia_web::coalesce_frame_runs(self.exact.keys())
    }

    /// Covered strip runs in **global** FrameIds.
    pub fn covered_ready_marks(&self) -> Vec<(FrameId, FrameId)> {
        fpmedia_web::coalesce_frame_runs(self.covered.iter().copied())
    }

    /// Drop all Exact values (caller releases) + clear Covered.
    pub fn take_all_exact(&mut self) -> Vec<(FrameId, V)> {
        self.covered.clear();
        self.exact.take_all().into_iter().collect()
    }

    pub fn clear_covered(&mut self) {
        self.covered.clear();
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
        let mut led = ExactLedger::<u64>::with_capacity(4);
        led.set_aim(10);
        for g in 8u64..=11 {
            let out = led.put_exact(g, g);
            assert!(out.accepted, "insert {g}");
            assert!(out.replaced.is_none());
            assert!(out.evicted.is_empty());
            assert!(out.refused.is_none());
        }
        assert_eq!(led.exact_len(), 4);
        // At cap, far from aim refused or evicts far end — still max 4.
        let out = led.put_exact(100, 100);
        if out.accepted {
            assert!(led.exact_len() <= 4);
        }
        assert!(led.exact_len() <= led.capacity());
    }

    #[test]
    fn hit_miss_and_aim_evict() {
        let mut led = ExactLedger::<u64>::with_capacity(3);
        led.set_aim(5);
        assert!(!led.has_exact(5));
        assert!(led.put_exact(5, 50).accepted);
        assert!(led.has_exact(5));
        assert_eq!(led.get_exact(5), Some(&50));

        assert!(led.put_exact(4, 40).accepted);
        assert!(led.put_exact(6, 60).accepted);
        assert_eq!(led.exact_len(), 3);

        // Far insert while only near-aim residents → refuse (same gate as scrub blit).
        let out = led.put_exact(20, 200);
        assert!(!out.accepted);
        assert!(out.refused.is_some());
        assert!(!led.has_exact(20));
        assert_eq!(led.exact_len(), 3);

        // Spread residents — insert near aim can evict the far end.
        let mut led = ExactLedger::<u64>::with_capacity(3);
        led.set_aim(50);
        assert!(led.put_exact(0, 0).accepted);
        assert!(led.put_exact(50, 50).accepted);
        assert!(led.put_exact(100, 100).accepted);
        let out = led.put_exact(51, 51);
        assert!(out.accepted);
        assert!(led.has_exact(51));
        assert_eq!(led.exact_len(), 3);
        assert!(!out.evicted.is_empty());
        assert!(!led.has_exact(0) || !led.has_exact(100));
    }

    #[test]
    fn covered_uncapped_across_joins() {
        let mut led = ExactLedger::<u64>::new();
        led.note_covered_run(0, 9); // clip A
        led.note_covered_run(10, 19); // clip B (global join)
        assert!(led.has_covered(0));
        assert!(led.has_covered(9));
        assert!(led.has_covered(10));
        assert!(led.has_covered(19));
        assert_eq!(led.covered_frame_ids().len(), 20);
        assert_eq!(led.covered_ready_marks(), vec![(0, 19)]);
    }

    #[test]
    fn default_cap_is_single_clip_floor() {
        let led = ExactLedger::<()>::new();
        assert_eq!(led.capacity(), MIN_SCRUB_BLIT_FRAMES);
    }

    #[test]
    fn exact_ready_marks_coalesce_global_runs() {
        let mut led = ExactLedger::<u64>::with_capacity(16);
        led.set_aim(5);
        for g in [1u64, 2, 3, 10, 11] {
            assert!(led.put_exact(g, g).accepted);
        }
        let runs = led.exact_ready_marks();
        assert_eq!(runs, vec![(1, 3), (10, 11)]);
    }

    /// Densify bulk sync must keep playhead aim fixed while inserting sorted ids.
    #[test]
    fn bulk_put_keeps_playhead_aim() {
        let mut led = ExactLedger::<u64>::with_capacity(8);
        led.set_aim(50);
        for g in 40u64..=47 {
            assert!(led.put_exact(g, g).accepted, "insert {g}");
            assert_eq!(led.aim(), 50, "aim must not walk with insert id");
        }
        assert!(led.has_exact(40));
        assert!(led.has_exact(47));
        assert_eq!(led.exact_len(), 8);
    }

    #[test]
    fn orphans_beyond_aim_drop_far_exact() {
        let mut led = ExactLedger::with_capacity(4);
        led.set_aim(0);
        for i in 0..4u64 {
            assert!(led.put_exact(i, i).accepted);
        }
        led.set_aim(100);
        let victims = led.take_orphans_beyond_aim();
        assert_eq!(victims.len(), 4);
        assert_eq!(led.exact_len(), 0);
    }
}
