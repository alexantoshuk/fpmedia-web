//! Pure densify gap / handle plan on **timeline** FrameIds.
//!
//! Reuses engine [`decide_next_gap`] / [`scrub_gap_fill_armed`] / [`AimWindow`] —
//! does not fork margin constants. Pinned edges = timeline `0` / `last` only;
//! clip joins mid-axis stay armed like mid-clip.
//! **no equivalent** (VT / scrub local).

use std::collections::BTreeSet;

use fpmedia_web::{
    AimWindow, FrameId, NextGapDecision, covered_edge_margins, decide_next_gap, scrub_edge_margins,
    scrub_gap_fill_armed,
};

/// Inputs for [`plan_next_densify_gap`] (native-testable; no bitmaps).
#[derive(Clone, Debug)]
pub struct DensifyPlanInput {
    /// Timeline aim (playhead).
    pub aim: FrameId,
    /// Last timeline FrameId (`frame_count - 1`).
    pub timeline_last: FrameId,
    /// Indexed last (fMP4); usually `== timeline_last` until warm open.
    pub indexed_last: FrameId,
    /// Exact AimWindow capacity (frames).
    pub exact_cap: usize,
    /// Resident Exact timeline FrameIds.
    pub exact: BTreeSet<FrameId>,
    /// Resident Covered timeline FrameIds.
    pub covered: BTreeSet<FrameId>,
    /// Exact edge margins (from `scrub_edge_margins` / ScrubPolicy).
    pub margin_left: u64,
    pub margin_right: u64,
    /// Scrub direction (−1 / 0 / +1).
    pub dir: i8,
    /// Stall aim — skip Exact retries at this aim (engine densify stall).
    pub fill_stalled_aim: Option<FrameId>,
    /// Timeline FrameIds to treat as Exact-filled (VT soft-ban / skip).
    pub skip_exact: BTreeSet<FrameId>,
}

impl DensifyPlanInput {
    /// Defaults: neutral dir, GOP-12 margins, no stall, indexed = timeline last.
    pub fn new(
        aim: FrameId,
        timeline_last: FrameId,
        exact_cap: usize,
        exact: BTreeSet<FrameId>,
        covered: BTreeSet<FrameId>,
    ) -> Self {
        let (margin_left, margin_right) = scrub_edge_margins(12);
        Self {
            aim,
            timeline_last,
            indexed_last: timeline_last,
            exact_cap,
            exact,
            covered,
            margin_left,
            margin_right,
            dir: 0,
            fill_stalled_aim: None,
            skip_exact: BTreeSet::new(),
        }
    }

    /// Build from VT ledger residency (timeline FrameIds).
    pub fn from_ledger<V>(
        ledger: &super::ExactLedger<V>,
        aim: FrameId,
        timeline_last: FrameId,
        margin_left: u64,
        margin_right: u64,
        dir: i8,
    ) -> Self {
        Self {
            aim,
            timeline_last,
            indexed_last: timeline_last,
            exact_cap: ledger.capacity(),
            exact: ledger.exact_frame_ids().into_iter().collect(),
            covered: ledger.covered_frame_ids().into_iter().collect(),
            margin_left,
            margin_right,
            dir,
            fill_stalled_aim: None,
            skip_exact: BTreeSet::new(),
        }
    }
}

/// Next timeline FrameId to densify, or `None` when saturated / disarmed.
///
/// Exact holes win over Covered-only; Covered gated by aim±band + edge arming
/// with **`timeline_last`** (clip joins are not EOF).
pub fn plan_next_densify_gap(input: &DensifyPlanInput) -> NextGapDecision {
    let last = input.timeline_last.min(input.indexed_last);
    let aim = input.aim.min(last);
    let cap = input.exact_cap.max(1);

    let blit_gap = {
        let mut exact = AimWindow::new(cap);
        exact.set_aim(aim);
        for &id in &input.exact {
            if id <= last {
                exact.insert(id, ());
            }
        }
        // Soft-banned / skip: treat as resident so plan does not re-pick them.
        for &id in &input.skip_exact {
            if id <= last {
                exact.insert(id, ());
            }
        }
        exact.capped_next_gap(aim, last, input.dir, input.margin_left, input.margin_right)
    };

    let covered: BTreeSet<FrameId> = input
        .covered
        .iter()
        .copied()
        .filter(|&id| id <= last)
        .collect();
    let extent = covered_extent(&covered);
    let no_gaps = covered_no_gaps(&covered, extent);
    let (cov_ml, cov_mr) = covered_edge_margins();
    let marker_armed = if !no_gaps {
        true
    } else {
        scrub_gap_fill_armed(true, aim, extent, last, cov_ml, cov_mr)
    };
    let marker_gap = {
        let mut cov = AimWindow::new(usize::MAX);
        cov.set_aim(aim);
        for &id in &covered {
            cov.insert(id, ());
        }
        cov.capped_next_gap(aim, last, input.dir, cov_ml, cov_mr)
    };
    let marker_saturated = marker_gap.is_none();

    decide_next_gap(
        aim,
        last,
        input.indexed_last.min(input.timeline_last),
        input.fill_stalled_aim,
        blit_gap,
        marker_saturated,
        marker_armed,
        marker_gap,
    )
}

fn covered_extent(covered: &BTreeSet<FrameId>) -> Option<(FrameId, FrameId)> {
    let lo = *covered.iter().next()?;
    let hi = *covered.iter().next_back()?;
    Some((lo, hi))
}

fn covered_no_gaps(covered: &BTreeSet<FrameId>, extent: Option<(FrameId, FrameId)>) -> bool {
    let Some((lo, hi)) = extent else {
        return false;
    };
    hi.saturating_sub(lo)
        .saturating_add(1)
        .eq(&(covered.len() as u64))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(ids: &[FrameId]) -> BTreeSet<FrameId> {
        ids.iter().copied().collect()
    }

    #[test]
    fn empty_exact_targets_aim() {
        let input = DensifyPlanInput::new(50, 199, 8, BTreeSet::new(), BTreeSet::new());
        let d = plan_next_densify_gap(&input);
        assert_eq!(d.target, Some(50));
    }

    #[test]
    fn mid_timeline_fills_nearer_hole() {
        // Symmetric island: gaps 48 and 52 are equal distance from aim 50.
        let exact = set(&[49, 50, 51]);
        let mut input = DensifyPlanInput::new(50, 199, 16, exact, BTreeSet::new());
        let d = plan_next_densify_gap(&input);
        assert!(
            matches!(d.target, Some(48) | Some(52)),
            "expected adjacent hole, got {:?}",
            d.target
        );
        input.dir = 1;
        assert_eq!(plan_next_densify_gap(&input).target, Some(52));
        input.dir = -1;
        assert_eq!(plan_next_densify_gap(&input).target, Some(48));
    }

    #[test]
    fn cut_is_not_pinned_like_clip_eof() {
        // Cap-full Exact ending at former clip end 99; timeline continues to 199.
        let exact = set(&(90..=99).collect::<Vec<_>>());
        let input = DensifyPlanInput::new(99, 199, 10, exact, BTreeSet::new());
        let d = plan_next_densify_gap(&input);
        assert_eq!(
            d.target,
            Some(100),
            "join must expand across cut (timeline last), got {:?}",
            d.target
        );
    }

    #[test]
    fn timeline_eof_does_not_expand_past_last() {
        let exact = set(&(90..=99).collect::<Vec<_>>());
        let input = DensifyPlanInput::new(99, 99, 10, exact, BTreeSet::new());
        let d = plan_next_densify_gap(&input);
        // Pinned right edge: may expand behind (89) or stop — never past 99.
        if let Some(t) = d.target {
            assert!(t <= 99, "past timeline last: {t}");
            assert_ne!(t, 100);
        }
    }

    #[test]
    fn timeline_start_expands_inward_near_extent_end() {
        let exact = set(&(0..=9).collect::<Vec<_>>());
        let input = DensifyPlanInput::new(8, 199, 10, exact, BTreeSet::new());
        let d = plan_next_densify_gap(&input);
        // Pinned left (start==0); near right of island → expand ahead only.
        assert_eq!(d.target, Some(10));
    }

    #[test]
    fn timeline_start_aim_at_origin_disarmed_when_residency_solid() {
        let ids = set(&(0..=9).collect::<Vec<_>>());
        // Covered must match Exact — empty Covered alone would target aim as Covered-only.
        let input = DensifyPlanInput::new(0, 199, 10, ids.clone(), ids);
        // Pinned left + aim not near island end → Exact+Covered both idle.
        assert_eq!(plan_next_densify_gap(&input).target, None);
    }
}
