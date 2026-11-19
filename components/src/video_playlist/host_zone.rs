//! Playlist aliases for VT LoopZone / Play wall helpers.
//!
//! Law lives in [`crate::virtual_timeline::loop_zone`]. Host still runs seams.
//! **no equivalent**.

// Re-export for wasm playlist host. Native builds host modules out — VT owns
// symbols + unit tests. `HostZoneOwner` = VT `ZoneOwner` (same as `LoopOwner`);
// call sites may use either alias — keep both for vocab.
#[allow(unused_imports)]
pub(crate) use crate::virtual_timeline::{
    AdvanceEndedCtx, AdvanceEndedPlan, HostOutsideCtx, HostOutsidePlan, PlayAtClipLastCtx,
    PlayAtClipLastPlan, ScrubUpCtx, ScrubUpPlan, SeamNextCtx, WrapDonePlan,
    ZoneOwner as HostZoneOwner, is_soft_seam_cancel, plan_advance_ended, plan_host_outside,
    plan_play_at_clip_last, plan_scrub_up, plan_seam_next_clip, plan_wrap_done,
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn soft_seam_cancel_alias() {
        assert!(is_soft_seam_cancel("operation cancelled"));
        assert!(!is_soft_seam_cancel("open timed out"));
    }

    #[test]
    fn host_alias_matches_vt_outside() {
        use crate::virtual_timeline::play_outside_zone;
        assert!(play_outside_zone(
            HostZoneOwner::Host,
            true,
            100,
            500,
            1000,
            50
        ));
    }
}
