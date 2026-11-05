//! Playlist host scrub/seam decisions — pure `plan_*`, UI runs cmds.
//!
//! Slice A: scrub-while-play Up. Slice B: PlayPressed / wall Effect Host outside.
//! Slice C: WrapDone soft/hard coalesce. Slice D: Next/Prev step / Goto.
//! Slice E: clip-EOF advance (`plan_advance_ended` / `plan_seam_next_clip`).
//! Slice F: Play@clip-last shares seam-next / mid-in wrap (`plan_play_at_clip_last`).
//! Design: `.ai/architecture.md` § Playlist host plans.
//! **no equivalent** (playlist local).

use super::host_zone::{HostZoneOwner, host_play_outside_zone, is_soft_seam_cancel};
use super::position::PlaylistPos;

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

/// Next / Prev frame chrome direction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
pub(crate) enum StepDir {
    Next,
    Prev,
}

/// Inputs for playlist Next/Prev (live local frame already read).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
pub(crate) struct StepCtx {
    pub dir: StepDir,
    pub clip_index: u32,
    pub local_frame: u32,
    /// Last presentation frame of the live clip (`frame_count − 1`).
    pub clip_last_frame: u32,
    pub clip_count: u32,
    pub looping: bool,
    /// Prev @0 → previous clip's last; Prev wrap → last clip's last; Next unused.
    pub edge_target_local: u32,
}

/// What Next/Prev must run — stop Play / ignore LoopZone (Goto, not seek).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
pub(crate) enum StepPlan {
    /// Clear facade intent + live slot `next_frame` / `previous_frame`.
    LiveStep { dir: StepDir },
    /// `goto_pos` (stop Play + internal seek). Ignores LoopZone.
    GotoPos { pos: PlaylistPos },
    /// Edge with loop off — clear facade Play only.
    ClearPlayIntent,
}

/// Shared law for Next/Prev chrome. Does not consult LoopZone.
#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
pub(crate) fn plan_step(ctx: StepCtx) -> StepPlan {
    match ctx.dir {
        StepDir::Next => {
            if ctx.local_frame < ctx.clip_last_frame {
                return StepPlan::LiveStep { dir: StepDir::Next };
            }
            let next = ctx.clip_index.saturating_add(1);
            if next < ctx.clip_count {
                return StepPlan::GotoPos {
                    pos: PlaylistPos::new(next, 0),
                };
            }
            if ctx.looping && ctx.clip_count > 0 {
                return StepPlan::GotoPos {
                    pos: PlaylistPos::new(0, 0),
                };
            }
            StepPlan::ClearPlayIntent
        }
        StepDir::Prev => {
            if ctx.local_frame > 0 {
                return StepPlan::LiveStep { dir: StepDir::Prev };
            }
            if ctx.clip_index > 0 {
                return StepPlan::GotoPos {
                    pos: PlaylistPos::new(ctx.clip_index - 1, ctx.edge_target_local),
                };
            }
            if ctx.looping && ctx.clip_count > 0 {
                let last_i = ctx.clip_count.saturating_sub(1);
                return StepPlan::GotoPos {
                    pos: PlaylistPos::new(last_i, ctx.edge_target_local),
                };
            }
            StepPlan::ClearPlayIntent
        }
    }
}

/// Inputs for Play-All / LoopZone next-clip pick after clip EOF (or warm schedule).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
pub(crate) struct SeamNextCtx {
    pub cur: u32,
    pub next_clip: Option<u32>,
    pub next_start: u32,
    /// Clip owning `zone_in` (if any).
    pub zone_in_clip: Option<u32>,
    pub looping: bool,
    pub zone_full: bool,
    pub zone_out: u32,
    pub clip_count: u32,
}

/// Which clip to promote after EOF — `None` → wrap or terminal (caller decides).
#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
pub(crate) fn plan_seam_next_clip(ctx: SeamNextCtx) -> Option<u32> {
    if let Some(next) = ctx.next_clip {
        if !ctx.looping || ctx.zone_full || ctx.next_start < ctx.zone_out {
            return Some(next);
        }
        // Next would leave the zone — wrap to zone-in clip (if different).
        return ctx.zone_in_clip.filter(|&c| c != ctx.cur);
    }
    if ctx.looping && ctx.clip_count > 1 {
        return ctx.zone_in_clip.or(Some(0));
    }
    None
}

/// Inputs for clip-EOF advance after `try_begin_seam` (live still `from_clip`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
pub(crate) struct AdvanceEndedCtx {
    pub live_still_from_clip: bool,
    pub owner: HostZoneOwner,
    pub zone_full: bool,
    pub zone_out: u32,
    /// Exclusive global end of `from_clip` (`clip_end_frame`).
    pub clip_end: u32,
    /// `(clip_index, local_frame)` at `zone_in`, if resolvable.
    pub zone_in_aim: Option<(u32, u32)>,
    pub seam_next: Option<u32>,
    pub looping: bool,
}

/// What `advance_after_ended` must run (wrap / promote@0 / terminal).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
pub(crate) enum AdvanceEndedPlan {
    /// Live already moved — drop.
    Noop,
    /// Host `wrap_to_loop_in`.
    Wrap,
    /// `promote_idle_at` at clip local 0.
    Promote { clip_index: u32 },
    /// Last clip, loop off — facade Ended.
    TerminalEnd,
}

/// Shared law for clip-EOF seam. Does not latch `SeamPhase` (Effect owns that).
#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
pub(crate) fn plan_advance_ended(ctx: AdvanceEndedCtx) -> AdvanceEndedPlan {
    if !ctx.live_still_from_clip {
        return AdvanceEndedPlan::Noop;
    }
    // LoopZone ends inside this clip: host wrap only when Host owns the wall.
    if matches!(ctx.owner, HostZoneOwner::Host) && !ctx.zone_full && ctx.zone_out <= ctx.clip_end {
        return AdvanceEndedPlan::Wrap;
    }
    if let Some(next_i) = ctx.seam_next {
        // Zone wrap to mid-clip in → not frame-0 origin seam.
        if matches!(ctx.owner, HostZoneOwner::Host)
            && !ctx.zone_full
            && let Some((aim_clip, aim_local)) = ctx.zone_in_aim
            && aim_clip == next_i
            && aim_local > 0
        {
            return AdvanceEndedPlan::Wrap;
        }
        return AdvanceEndedPlan::Promote { clip_index: next_i };
    }
    if ctx.looping {
        return AdvanceEndedPlan::Wrap;
    }
    AdvanceEndedPlan::TerminalEnd
}

/// Play pressed at live clip last while still inside Host zone (not outside-wall).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
pub(crate) struct PlayAtClipLastCtx {
    pub zone_full: bool,
    pub zone_in_aim: Option<(u32, u32)>,
    pub seam_next: Option<u32>,
    pub seam_busy: bool,
}

/// Avoid Host slot PlayPrime snap→0: promote next@0 or wrap to `loop_in`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
pub(crate) enum PlayAtClipLastPlan {
    /// `seek_to_pos` clip@0 with then_play.
    PromoteSeek { clip_index: u32 },
    /// Mid `zone_in` or no next — `start_host_wrap`.
    HostWrap { seam_already_busy: bool },
}

/// Same mid-in / next pick as EOF advance; cmds differ (seek+play vs promote_idle).
#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
pub(crate) fn plan_play_at_clip_last(ctx: PlayAtClipLastCtx) -> PlayAtClipLastPlan {
    if let Some(next_i) = ctx.seam_next {
        if !ctx.zone_full
            && let Some((aim_clip, aim_local)) = ctx.zone_in_aim
            && aim_clip == next_i
            && aim_local > 0
        {
            return PlayAtClipLastPlan::HostWrap {
                seam_already_busy: ctx.seam_busy,
            };
        }
        return PlayAtClipLastPlan::PromoteSeek { clip_index: next_i };
    }
    PlayAtClipLastPlan::HostWrap {
        seam_already_busy: ctx.seam_busy,
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

    fn step_next(local: u32, last: u32, clip: u32, count: u32, looping: bool) -> StepCtx {
        StepCtx {
            dir: StepDir::Next,
            clip_index: clip,
            local_frame: local,
            clip_last_frame: last,
            clip_count: count,
            looping,
            edge_target_local: 0,
        }
    }

    fn step_prev(local: u32, clip: u32, count: u32, looping: bool, edge_local: u32) -> StepCtx {
        StepCtx {
            dir: StepDir::Prev,
            clip_index: clip,
            local_frame: local,
            clip_last_frame: 99,
            clip_count: count,
            looping,
            edge_target_local: edge_local,
        }
    }

    #[test]
    fn step_next_mid_live_edge_goto_or_clear() {
        assert_eq!(
            plan_step(step_next(10, 99, 0, 2, false)),
            StepPlan::LiveStep { dir: StepDir::Next }
        );
        assert_eq!(
            plan_step(step_next(99, 99, 0, 2, false)),
            StepPlan::GotoPos {
                pos: PlaylistPos::new(1, 0)
            }
        );
        assert_eq!(
            plan_step(step_next(99, 99, 1, 2, true)),
            StepPlan::GotoPos {
                pos: PlaylistPos::new(0, 0)
            }
        );
        assert_eq!(
            plan_step(step_next(99, 99, 1, 2, false)),
            StepPlan::ClearPlayIntent
        );
    }

    #[test]
    fn step_prev_mid_live_edge_goto_or_clear() {
        assert_eq!(
            plan_step(step_prev(5, 1, 2, false, 0)),
            StepPlan::LiveStep { dir: StepDir::Prev }
        );
        assert_eq!(
            plan_step(step_prev(0, 1, 2, false, 40)),
            StepPlan::GotoPos {
                pos: PlaylistPos::new(0, 40)
            }
        );
        assert_eq!(
            plan_step(step_prev(0, 0, 2, true, 77)),
            StepPlan::GotoPos {
                pos: PlaylistPos::new(1, 77)
            }
        );
        assert_eq!(
            plan_step(step_prev(0, 0, 2, false, 0)),
            StepPlan::ClearPlayIntent
        );
    }

    #[test]
    fn seam_next_in_zone_vs_wrap_to_in_clip() {
        // Sequential next inside zone.
        assert_eq!(
            plan_seam_next_clip(SeamNextCtx {
                cur: 0,
                next_clip: Some(1),
                next_start: 100,
                zone_in_clip: Some(0),
                looping: true,
                zone_full: false,
                zone_out: 500,
                clip_count: 3,
            }),
            Some(1)
        );
        // Next starts at/after zone_out → zone_in clip if different.
        assert_eq!(
            plan_seam_next_clip(SeamNextCtx {
                cur: 0,
                next_clip: Some(1),
                next_start: 500,
                zone_in_clip: Some(2),
                looping: true,
                zone_full: false,
                zone_out: 500,
                clip_count: 3,
            }),
            Some(2)
        );
        // Last clip + looping multi → zone_in (or 0).
        assert_eq!(
            plan_seam_next_clip(SeamNextCtx {
                cur: 1,
                next_clip: None,
                next_start: 0,
                zone_in_clip: Some(0),
                looping: true,
                zone_full: true,
                zone_out: 1000,
                clip_count: 2,
            }),
            Some(0)
        );
        assert_eq!(
            plan_seam_next_clip(SeamNextCtx {
                cur: 1,
                next_clip: None,
                next_start: 0,
                zone_in_clip: None,
                looping: false,
                zone_full: true,
                zone_out: 1000,
                clip_count: 2,
            }),
            None
        );
    }

    fn advance_host(seam_next: Option<u32>, zone_out: u32, clip_end: u32) -> AdvanceEndedCtx {
        AdvanceEndedCtx {
            live_still_from_clip: true,
            owner: HostZoneOwner::Host,
            zone_full: false,
            zone_out,
            clip_end,
            zone_in_aim: Some((0, 10)),
            seam_next,
            looping: true,
        }
    }

    #[test]
    fn advance_ended_wrap_promote_terminal() {
        assert_eq!(
            plan_advance_ended(AdvanceEndedCtx {
                live_still_from_clip: false,
                ..advance_host(Some(1), 500, 200)
            }),
            AdvanceEndedPlan::Noop
        );
        // Zone wall inside this clip → Wrap.
        assert_eq!(
            plan_advance_ended(advance_host(Some(1), 150, 200)),
            AdvanceEndedPlan::Wrap
        );
        // Promote next @0.
        assert_eq!(
            plan_advance_ended(AdvanceEndedCtx {
                zone_in_aim: Some((0, 0)),
                ..advance_host(Some(1), 500, 200)
            }),
            AdvanceEndedPlan::Promote { clip_index: 1 }
        );
        // Next is mid zone_in → Wrap.
        assert_eq!(
            plan_advance_ended(AdvanceEndedCtx {
                seam_next: Some(0),
                zone_in_aim: Some((0, 10)),
                ..advance_host(Some(0), 500, 200)
            }),
            AdvanceEndedPlan::Wrap
        );
        assert_eq!(
            plan_advance_ended(AdvanceEndedCtx {
                seam_next: None,
                looping: true,
                ..advance_host(None, 500, 200)
            }),
            AdvanceEndedPlan::Wrap
        );
        assert_eq!(
            plan_advance_ended(AdvanceEndedCtx {
                seam_next: None,
                looping: false,
                owner: HostZoneOwner::Host,
                zone_full: true,
                live_still_from_clip: true,
                zone_out: 1000,
                clip_end: 1000,
                zone_in_aim: None,
            }),
            AdvanceEndedPlan::TerminalEnd
        );
    }

    #[test]
    fn play_at_clip_last_promote_or_wrap() {
        assert_eq!(
            plan_play_at_clip_last(PlayAtClipLastCtx {
                zone_full: false,
                zone_in_aim: Some((0, 0)),
                seam_next: Some(1),
                seam_busy: false,
            }),
            PlayAtClipLastPlan::PromoteSeek { clip_index: 1 }
        );
        assert_eq!(
            plan_play_at_clip_last(PlayAtClipLastCtx {
                zone_full: false,
                zone_in_aim: Some((0, 10)),
                seam_next: Some(0),
                seam_busy: true,
            }),
            PlayAtClipLastPlan::HostWrap {
                seam_already_busy: true
            }
        );
        assert_eq!(
            plan_play_at_clip_last(PlayAtClipLastCtx {
                zone_full: false,
                zone_in_aim: Some((0, 10)),
                seam_next: None,
                seam_busy: false,
            }),
            PlayAtClipLastPlan::HostWrap {
                seam_already_busy: false
            }
        );
    }
}
