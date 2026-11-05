//! Playlist host scrub/seam decisions — sync plan, UI runs cmds.
//!
//! Slice A: scrub-while-play Up. Further events (PlayPressed, wall Effect) land
//! here later. Design: `.ai/architecture.md` § Playlist host reduce.
//! **no equivalent** (playlist local).

use super::host_zone::{HostZoneOwner, host_play_outside_zone};

/// Inputs for scrub pointer-up (global aim already committed on the facade).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
pub(crate) struct ScrubUpCtx {
    pub global: u32,
    pub resume: bool,
    pub aim_on_live: bool,
    pub owner: HostZoneOwner,
    pub looping: bool,
    pub zone_in: u32,
    pub zone_out: u32,
    pub total_frames: u32,
    /// `SeamPhase` already busy — do not steal; keep facade Play for wall re-entry.
    pub seam_busy: bool,
}

/// What the host must run after scrub Up (no Leptos / no spawn in this module).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
pub(crate) enum ScrubUpPlan {
    /// Leave scrub without Exact@outside, then Host wrap to `loop_in`.
    HostWrap {
        /// Seam already Preparing/Swapping/Playing — only keep facade Play.
        seam_already_busy: bool,
    },
    /// Aim clip is live: slot `scrub_pointer_up`, optional facade play + idle preload.
    LiveSettle { then_play: bool },
    /// Aim clip not live: `seek_to_pos` / `seek_pos` (open/swap).
    CrossClipSeek { then_play: bool },
}

/// Scrub-while-play Up outside Host zone → wrap; else normal settle / seek.
///
/// Law: never Exact-settle / seek-play at an outside aim when Host wrap applies
/// (that raced `prepare_playlist_seam_at` → sticky `operation cancelled`).
#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
pub(crate) fn plan_scrub_up(ctx: ScrubUpCtx) -> ScrubUpPlan {
    if ctx.resume
        && host_play_outside_zone(
            ctx.owner,
            ctx.looping,
            ctx.zone_in,
            ctx.zone_out,
            ctx.total_frames,
            ctx.global,
        )
    {
        return ScrubUpPlan::HostWrap {
            seam_already_busy: ctx.seam_busy,
        };
    }
    if ctx.aim_on_live {
        ScrubUpPlan::LiveSettle {
            then_play: ctx.resume,
        }
    } else {
        ScrubUpPlan::CrossClipSeek {
            then_play: ctx.resume,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn host_zone(global: u32, resume: bool, on_live: bool, seam_busy: bool) -> ScrubUpCtx {
        ScrubUpCtx {
            global,
            resume,
            aim_on_live: on_live,
            owner: HostZoneOwner::Host,
            looping: true,
            zone_in: 100,
            zone_out: 500,
            total_frames: 1000,
            seam_busy,
        }
    }

    #[test]
    fn scrub_up_resume_outside_plans_host_wrap() {
        assert_eq!(
            plan_scrub_up(host_zone(50, true, true, false)),
            ScrubUpPlan::HostWrap {
                seam_already_busy: false
            }
        );
        assert_eq!(
            plan_scrub_up(host_zone(50, true, false, true)),
            ScrubUpPlan::HostWrap {
                seam_already_busy: true
            }
        );
        // No Exact settle / cross-clip seek when wrap applies.
        assert!(!matches!(
            plan_scrub_up(host_zone(50, true, true, false)),
            ScrubUpPlan::LiveSettle { .. } | ScrubUpPlan::CrossClipSeek { .. }
        ));
    }

    #[test]
    fn scrub_up_paused_outside_settles_normally() {
        assert_eq!(
            plan_scrub_up(host_zone(50, false, true, false)),
            ScrubUpPlan::LiveSettle { then_play: false }
        );
        assert_eq!(
            plan_scrub_up(host_zone(50, false, false, false)),
            ScrubUpPlan::CrossClipSeek { then_play: false }
        );
    }

    #[test]
    fn scrub_up_resume_inside_plays_via_settle_or_seek() {
        assert_eq!(
            plan_scrub_up(host_zone(250, true, true, false)),
            ScrubUpPlan::LiveSettle { then_play: true }
        );
        assert_eq!(
            plan_scrub_up(host_zone(250, true, false, false)),
            ScrubUpPlan::CrossClipSeek { then_play: true }
        );
    }

    #[test]
    fn scrub_up_engine_owner_outside_does_not_host_wrap() {
        let mut ctx = host_zone(50, true, true, false);
        ctx.owner = HostZoneOwner::Engine;
        assert_eq!(
            plan_scrub_up(ctx),
            ScrubUpPlan::LiveSettle { then_play: true }
        );
    }
}
