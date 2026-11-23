//! Playlist host scrub/seam decisions — pure `plan_*`, UI runs cmds.
//!
//! Slices A–C + E–F LoopZone/Play wall law: VT [`crate::virtual_timeline::loop_zone`]
//! (re-exported). Slice D: Next/Prev step / Goto (stays here — `PlaylistPos`).
//! Design: `.ai/architecture.md` § Playlist host plans / Virtual Timeline.
//! **no equivalent** (playlist local).

use super::position::PlaylistPos;
use crate::frame_axis::ClipFrame;

// Same as `host_zone`: wasm call sites; native only builds `plan_step` + VT tests.
#[cfg_attr(not(target_arch = "wasm32"), allow(unused_imports))]
pub(crate) use super::host_zone::{
    AdvanceEndedCtx, AdvanceEndedPlan, HostOutsideCtx, HostOutsidePlan, PlayAtClipLastCtx,
    PlayAtClipLastPlan, ScrubUpCtx, ScrubUpPlan, SeamNextCtx, WrapDonePlan, plan_advance_ended,
    plan_host_outside, plan_play_at_clip_last, plan_scrub_up, plan_seam_next_clip, plan_wrap_done,
};

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
    pub frame: ClipFrame,
    /// Last presentation frame of the live clip (`frame_count − 1`).
    pub clip_last_frame: ClipFrame,
    pub clip_count: u32,
    pub looping: bool,
    /// Prev @0 → previous clip's last; Prev wrap → last clip's last; Next unused.
    pub edge_target_local: ClipFrame,
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

/// Where a cross-clip seek / promote may arm a seam after `ensure_clip_on_idle`.
///
/// A newer tap `close()`s the idle slot before its player exists. Preparing
/// that slot surfaces sticky `media source: no player`. **no equivalent**.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
pub(crate) enum IdlePreparePlan {
    /// Aim clip is on the live slot and that slot is ready. Do not prepare idle.
    Live,
    /// Aim clip is on the idle slot and that slot is ready. SeamHold / Exact, then swap.
    Idle,
    /// Aim is on neither ready slot — superseded open. `Cancelled`, no banner.
    Superseded,
}

/// Slot residency after `ensure_clip_on_idle` (signals, not the captured `VideoRef`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
pub(crate) struct IdlePrepareCtx {
    pub live_clip: u32,
    pub live_ready: bool,
    /// `ready` can stay true for a tick after `retire_clip` takes the player.
    pub live_has_player: bool,
    pub idle_clip: Option<u32>,
    pub idle_ready: bool,
    pub idle_has_player: bool,
    pub aim_clip: u32,
}

/// Live+player wins over idle so a promote during ensure does not arm the closed slot.
#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
pub(crate) fn plan_idle_prepare(ctx: IdlePrepareCtx) -> IdlePreparePlan {
    if ctx.live_clip == ctx.aim_clip && ctx.live_ready && ctx.live_has_player {
        IdlePreparePlan::Live
    } else if ctx.idle_clip == Some(ctx.aim_clip) && ctx.idle_ready && ctx.idle_has_player {
        IdlePreparePlan::Idle
    } else {
        IdlePreparePlan::Superseded
    }
}

/// Shared law for Next/Prev chrome. Does not consult LoopZone.
#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
pub(crate) fn plan_step(ctx: StepCtx) -> StepPlan {
    match ctx.dir {
        StepDir::Next => {
            if ctx.frame < ctx.clip_last_frame {
                return StepPlan::LiveStep { dir: StepDir::Next };
            }
            let next = ctx.clip_index.saturating_add(1);
            if next < ctx.clip_count {
                return StepPlan::GotoPos {
                    pos: PlaylistPos::new(next, ClipFrame::new(0)),
                };
            }
            if ctx.looping && ctx.clip_count > 0 {
                return StepPlan::GotoPos {
                    pos: PlaylistPos::new(0, ClipFrame::new(0)),
                };
            }
            StepPlan::ClearPlayIntent
        }
        StepDir::Prev => {
            if ctx.frame.get() > 0 {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn step_next(local: u32, last: u32, clip: u32, count: u32, looping: bool) -> StepCtx {
        StepCtx {
            dir: StepDir::Next,
            clip_index: clip,
            frame: ClipFrame::new(local),
            clip_last_frame: ClipFrame::new(last),
            clip_count: count,
            looping,
            edge_target_local: ClipFrame::new(0),
        }
    }

    fn step_prev(local: u32, clip: u32, count: u32, looping: bool, edge_local: u32) -> StepCtx {
        StepCtx {
            dir: StepDir::Prev,
            clip_index: clip,
            frame: ClipFrame::new(local),
            clip_last_frame: ClipFrame::new(99),
            clip_count: count,
            looping,
            edge_target_local: ClipFrame::new(edge_local),
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
                pos: PlaylistPos::new(1, ClipFrame::new(0))
            }
        );
        assert_eq!(
            plan_step(step_next(99, 99, 1, 2, true)),
            StepPlan::GotoPos {
                pos: PlaylistPos::new(0, ClipFrame::new(0))
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
                pos: PlaylistPos::new(0, ClipFrame::new(40))
            }
        );
        assert_eq!(
            plan_step(step_prev(0, 0, 2, true, 77)),
            StepPlan::GotoPos {
                pos: PlaylistPos::new(1, ClipFrame::new(77))
            }
        );
        assert_eq!(
            plan_step(step_prev(0, 0, 2, false, 0)),
            StepPlan::ClearPlayIntent
        );
    }

    fn prep(
        live_clip: u32,
        live_ready: bool,
        live_has_player: bool,
        idle_clip: Option<u32>,
        idle_ready: bool,
        idle_has_player: bool,
        aim_clip: u32,
    ) -> IdlePrepareCtx {
        IdlePrepareCtx {
            live_clip,
            live_ready,
            live_has_player,
            idle_clip,
            idle_ready,
            idle_has_player,
            aim_clip,
        }
    }

    #[test]
    fn idle_prepare_live_idle_or_superseded() {
        assert_eq!(
            plan_idle_prepare(prep(0, true, true, Some(1), true, true, 0)),
            IdlePreparePlan::Live
        );
        assert_eq!(
            plan_idle_prepare(prep(0, true, true, Some(1), true, true, 1)),
            IdlePreparePlan::Idle
        );
        // Closed idle (rapid next tap) must not look ready.
        assert_eq!(
            plan_idle_prepare(prep(0, true, true, Some(1), false, false, 1)),
            IdlePreparePlan::Superseded
        );
        // `ready` still true after the player was taken.
        assert_eq!(
            plan_idle_prepare(prep(0, true, true, Some(1), true, false, 1)),
            IdlePreparePlan::Superseded
        );
        // Idle holds a different clip than the aim.
        assert_eq!(
            plan_idle_prepare(prep(0, true, true, Some(2), true, true, 1)),
            IdlePreparePlan::Superseded
        );
        // Aim became live, but that slot was retired — not a prepare target.
        assert_eq!(
            plan_idle_prepare(prep(1, false, false, None, false, false, 1)),
            IdlePreparePlan::Superseded
        );
    }
}
