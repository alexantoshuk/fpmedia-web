//! Playlist host scrub/seam decisions — sync plan, UI runs cmds.
//!
//! Slice A: scrub-while-play Up. Slice B: PlayPressed / wall Effect Host outside.
//! Slice C: WrapDone soft/hard coalesce.
//! Design: `.ai/architecture.md` § Playlist host reduce.
//! **no equivalent** (playlist local).

use super::host_zone::{HostZoneOwner, host_play_outside_zone, is_soft_seam_cancel};

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

/// Play / wall: is global aim outside the Host LoopZone?
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
pub(crate) struct HostOutsideCtx {
    pub owner: HostZoneOwner,
    pub looping: bool,
    pub zone_in: u32,
    pub zone_out: u32,
    pub total_frames: u32,
    pub global: u32,
    pub seam_busy: bool,
    /// Effect past-out: only when `zone_last` lies on the **live** clip (clip-EOF
    /// advance owns the join otherwise). PlayPressed uses `false` (full axis check).
    pub past_out_needs_wall_on_live: bool,
    pub wall_on_live: bool,
}

/// Host outside while wanting Play → wrap; Engine outside → slot play; else none.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
pub(crate) enum HostOutsidePlan {
    /// Begin seam + `wrap_to_loop_in` (or keep facade Play if busy).
    HostWrap { seam_already_busy: bool },
    /// Zone ⊆ live — engine `LoopSeam` / PlayPrime owns the wall.
    EngineSlotPlay,
    /// Not an outside-Play wall case (fall through to clip-EOF / bare play).
    NotOutside,
}

/// Shared law for PlayPressed outside + wall Effect Host wrap.
#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
pub(crate) fn plan_host_outside(ctx: HostOutsideCtx) -> HostOutsidePlan {
    if !ctx.looping || ctx.zone_out <= ctx.zone_in {
        return HostOutsidePlan::NotOutside;
    }
    // Full axis — not Host outside-wrap (same as `host_play_outside_zone`).
    if ctx.zone_in == 0 && ctx.zone_out == ctx.total_frames {
        return HostOutsidePlan::NotOutside;
    }
    let zone_last = ctx.zone_out.saturating_sub(1);
    let before_in = ctx.global < ctx.zone_in;
    let past_out = if ctx.past_out_needs_wall_on_live {
        ctx.wall_on_live && ctx.global >= zone_last
    } else {
        ctx.global >= zone_last
    };
    if !(before_in || past_out) {
        return HostOutsidePlan::NotOutside;
    }
    match ctx.owner {
        HostZoneOwner::Host => HostOutsidePlan::HostWrap {
            seam_already_busy: ctx.seam_busy,
        },
        HostZoneOwner::Engine => HostOutsidePlan::EngineSlotPlay,
        HostZoneOwner::Off => HostOutsidePlan::NotOutside,
    }
}

/// After Host wrap / seam async returns — soft cancel vs sticky hard fail.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
pub(crate) enum WrapDonePlan {
    /// Wrap / seam succeeded — no facade error path.
    Ok,
    /// Soft supersede: resume live if facade still wants Play; no sticky error.
    ResumeLiveKeepPlay,
    /// Hard failure — clear facade Play + surface error (caller has the string).
    HardFail,
    /// `open_gen` mismatch — ignore.
    Ignore,
}

/// One law for wrap/seam Err after live was already stopped.
#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
pub(crate) fn plan_wrap_done(result: Result<(), &str>, epoch_ok: bool) -> WrapDonePlan {
    if !epoch_ok {
        return WrapDonePlan::Ignore;
    }
    match result {
        Ok(()) => WrapDonePlan::Ok,
        Err(e) if is_soft_seam_cancel(e) => WrapDonePlan::ResumeLiveKeepPlay,
        Err(_) => WrapDonePlan::HardFail,
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

    fn outside_ctx(global: u32, wall_on_live: bool, need_wall: bool) -> HostOutsideCtx {
        HostOutsideCtx {
            owner: HostZoneOwner::Host,
            looping: true,
            zone_in: 100,
            zone_out: 500,
            total_frames: 1000,
            global,
            seam_busy: false,
            past_out_needs_wall_on_live: need_wall,
            wall_on_live,
        }
    }

    #[test]
    fn play_pressed_outside_host_wraps_engine_plays() {
        assert_eq!(
            plan_host_outside(outside_ctx(50, false, false)),
            HostOutsidePlan::HostWrap {
                seam_already_busy: false
            }
        );
        assert_eq!(
            plan_host_outside(outside_ctx(499, false, false)),
            HostOutsidePlan::HostWrap {
                seam_already_busy: false
            }
        );
        let mut eng = outside_ctx(50, false, false);
        eng.owner = HostZoneOwner::Engine;
        assert_eq!(plan_host_outside(eng), HostOutsidePlan::EngineSlotPlay);
        assert_eq!(
            plan_host_outside(outside_ctx(250, false, false)),
            HostOutsidePlan::NotOutside
        );
    }

    #[test]
    fn wall_effect_past_out_requires_wall_on_live() {
        // Past out but zone_last not on live → Effect must not Host-wrap (EOF advance).
        assert_eq!(
            plan_host_outside(outside_ctx(499, false, true)),
            HostOutsidePlan::NotOutside
        );
        assert_eq!(
            plan_host_outside(outside_ctx(499, true, true)),
            HostOutsidePlan::HostWrap {
                seam_already_busy: false
            }
        );
        // Before-in never needs wall_on_live.
        assert_eq!(
            plan_host_outside(outside_ctx(50, false, true)),
            HostOutsidePlan::HostWrap {
                seam_already_busy: false
            }
        );
    }

    #[test]
    fn wrap_done_soft_hard_epoch() {
        assert_eq!(plan_wrap_done(Ok(()), true), WrapDonePlan::Ok);
        assert_eq!(
            plan_wrap_done(Err("operation cancelled"), true),
            WrapDonePlan::ResumeLiveKeepPlay
        );
        assert_eq!(
            plan_wrap_done(Err("cancelled"), true),
            WrapDonePlan::ResumeLiveKeepPlay
        );
        assert_eq!(
            plan_wrap_done(Err("LoopZone in out of range"), true),
            WrapDonePlan::HardFail
        );
        assert_eq!(
            plan_wrap_done(Err("operation cancelled"), false),
            WrapDonePlan::Ignore
        );
        assert_eq!(plan_wrap_done(Ok(()), false), WrapDonePlan::Ignore);
    }
}
