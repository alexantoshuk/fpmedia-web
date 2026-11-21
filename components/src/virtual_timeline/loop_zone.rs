//! Virtual Timeline — LoopZone wall + Play/scrub-outside + seam-next / wrap-done
//! + SeamPhase FSM (slices 1–3).
//!
//! Pure global-axis law. Playlist host still **executes** seams (`wrap_to_loop_in`
//! / promote + signal I/O). Next/Prev step plans stay playlist-local
//! (`PlaylistPos`).
//! Design: `.ai/architecture.md` § Virtual Timeline / Playlist host plans.
//! **no equivalent** (playlist / scrub local).

use fpmedia_web::{FrameId, LoopZone};

/// Who arms LoopZone wrap for the current zone vs live span.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ZoneOwner {
    /// Looping off.
    Off,
    /// Zone ⊆ live clip → engine `LoopSeam`.
    Engine,
    /// Cross-clip / multi-clip wall → host promote / wrap.
    Host,
}

/// Host playlist seam in flight (cut promote or LoopZone wrap).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum SeamPhase {
    #[default]
    Idle,
    Preparing,
    Swapping,
    Playing,
}

impl SeamPhase {
    pub fn is_busy(self) -> bool {
        !matches!(self, Self::Idle)
    }
}

/// Derive owner from looping + global zone vs live clip half-open span `[start, end)`.
///
/// Does not read slot `Video.looping` — that is a sync side-effect on the host.
pub fn zone_owner_from_spans(
    looping: bool,
    zone_in: u32,
    zone_out: u32,
    clip_start: u32,
    clip_end: u32,
) -> ZoneOwner {
    if !looping {
        return ZoneOwner::Off;
    }
    if zone_in >= clip_start && zone_out <= clip_end && zone_out > zone_in {
        ZoneOwner::Engine
    } else {
        ZoneOwner::Host
    }
}

/// Idle → Preparing. `None` if a seam is already in flight.
pub fn seam_try_begin(phase: SeamPhase) -> Option<SeamPhase> {
    if phase.is_busy() {
        None
    } else {
        Some(SeamPhase::Preparing)
    }
}

/// Begin if Idle; keep Preparing; reject Swapping/Playing (`None`).
pub fn seam_ensure_preparing(phase: SeamPhase) -> Option<SeamPhase> {
    match phase {
        SeamPhase::Idle => seam_try_begin(phase),
        SeamPhase::Preparing => Some(SeamPhase::Preparing),
        SeamPhase::Swapping | SeamPhase::Playing => None,
    }
}

/// Host scrub-while-play / Play wall: aim is outside the half-open zone
/// (`g < in` or `g ≥ out−1`). Full-axis zone never wraps here.
pub fn play_outside_zone(
    owner: ZoneOwner,
    looping: bool,
    zone_in: u32,
    zone_out: u32,
    total_frames: u32,
    global: u32,
) -> bool {
    if !looping || !matches!(owner, ZoneOwner::Host) {
        return false;
    }
    if zone_out <= zone_in {
        return false;
    }
    // Full axis — same as pre-zone last→0 / engine path; not Host outside-wrap.
    if zone_in == 0 && zone_out == total_frames {
        return false;
    }
    let zone_last = zone_out.saturating_sub(1);
    global < zone_in || global >= zone_last
}

/// Inputs for scrub pointer-up (global aim already committed on the facade).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScrubUpCtx {
    pub global: u32,
    pub resume: bool,
    pub aim_on_live: bool,
    pub owner: ZoneOwner,
    pub looping: bool,
    pub zone_in: u32,
    pub zone_out: u32,
    pub total_frames: u32,
    /// Seam already busy — do not steal; keep facade Play for wall re-entry.
    pub seam_busy: bool,
}

/// What the host must run after scrub Up (no Leptos / no spawn).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScrubUpPlan {
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
pub fn plan_scrub_up(ctx: ScrubUpCtx) -> ScrubUpPlan {
    if ctx.resume
        && play_outside_zone(
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
pub struct HostOutsideCtx {
    pub owner: ZoneOwner,
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
pub enum HostOutsidePlan {
    /// Begin seam + `wrap_to_loop_in` (or keep facade Play if busy).
    HostWrap { seam_already_busy: bool },
    /// Zone ⊆ live — engine `LoopSeam` / PlayPrime owns the wall.
    EngineSlotPlay,
    /// Not an outside-Play wall case (fall through to clip-EOF / bare play).
    NotOutside,
}

/// Shared law for PlayPressed outside + wall Effect Host wrap.
pub fn plan_host_outside(ctx: HostOutsideCtx) -> HostOutsidePlan {
    if !ctx.looping || ctx.zone_out <= ctx.zone_in {
        return HostOutsidePlan::NotOutside;
    }
    // Full axis — not Host outside-wrap (same as `play_outside_zone`).
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
        ZoneOwner::Host => HostOutsidePlan::HostWrap {
            seam_already_busy: ctx.seam_busy,
        },
        ZoneOwner::Engine => HostOutsidePlan::EngineSlotPlay,
        ZoneOwner::Off => HostOutsidePlan::NotOutside,
    }
}

/// Soft op supersede (`MediaError::Cancelled` / epoch) — not a sticky seam failure.
pub fn is_soft_seam_cancel(err: &str) -> bool {
    let e = err.trim();
    e == "operation cancelled" || e.eq_ignore_ascii_case("cancelled")
}

/// After Host wrap / seam async returns — soft cancel vs sticky hard fail.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WrapDonePlan {
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
pub fn plan_wrap_done(result: Result<(), &str>, epoch_ok: bool) -> WrapDonePlan {
    if !epoch_ok {
        return WrapDonePlan::Ignore;
    }
    match result {
        Ok(()) => WrapDonePlan::Ok,
        Err(e) if is_soft_seam_cancel(e) => WrapDonePlan::ResumeLiveKeepPlay,
        Err(_) => WrapDonePlan::HardFail,
    }
}

/// Inputs for Play-All / LoopZone next-clip pick after clip EOF (or warm schedule).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SeamNextCtx {
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
pub fn plan_seam_next_clip(ctx: SeamNextCtx) -> Option<u32> {
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
pub struct AdvanceEndedCtx {
    pub live_still_from_clip: bool,
    pub owner: ZoneOwner,
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
pub enum AdvanceEndedPlan {
    /// Live already moved — drop.
    Noop,
    /// Host `wrap_to_loop_in`.
    Wrap,
    /// `promote_idle_at` at clip local 0.
    Promote { clip_index: u32 },
    /// Last clip, loop off — facade Ended.
    TerminalEnd,
}

/// Shared law for clip-EOF seam. Does not latch `SeamPhase` (host owns that).
pub fn plan_advance_ended(ctx: AdvanceEndedCtx) -> AdvanceEndedPlan {
    if !ctx.live_still_from_clip {
        return AdvanceEndedPlan::Noop;
    }
    // LoopZone ends inside this clip: host wrap only when Host owns the wall.
    if matches!(ctx.owner, ZoneOwner::Host) && !ctx.zone_full && ctx.zone_out <= ctx.clip_end {
        return AdvanceEndedPlan::Wrap;
    }
    if let Some(next_i) = ctx.seam_next {
        // Zone wrap to mid-clip in → not frame-0 origin seam.
        if matches!(ctx.owner, ZoneOwner::Host)
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
pub struct PlayAtClipLastCtx {
    pub zone_full: bool,
    pub zone_in_aim: Option<(u32, u32)>,
    pub seam_next: Option<u32>,
    pub seam_busy: bool,
}

/// Avoid Host slot PlayPrime snap→0: promote next@0 or wrap to `loop_in`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlayAtClipLastPlan {
    /// `seek_to_pos` clip@0 with then_play.
    PromoteSeek { clip_index: u32 },
    /// Mid `zone_in` or no next — `start_host_wrap`.
    HostWrap { seam_already_busy: bool },
}

/// Same mid-in / next pick as EOF advance; cmds differ (seek+play vs promote_idle).
pub fn plan_play_at_clip_last(ctx: PlayAtClipLastCtx) -> PlayAtClipLastPlan {
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

/// Zone as written to `loop_in` / `loop_out` signals.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StoredZone {
    pub in_frame: u32,
    pub out_frame: u32,
    /// `None` = full axis (or empty clip) — `loop_out` signal stays unset.
    pub loop_out: Option<u32>,
}

/// Clamp a requested zone and derive what the signals should store.
pub fn store_zone(count: u32, in_frame: u32, out_frame: u32) -> StoredZone {
    let LoopZone {
        in_frame: in_f,
        out_frame: out_f,
    } = LoopZone::normalize(in_frame, out_frame, count);
    let full = out_f == 0 || (in_f == 0 && out_f == count);
    StoredZone {
        in_frame: in_f,
        out_frame: out_f,
        loop_out: if full { None } else { Some(out_f) },
    }
}

/// Resolve stored signals (`loop_out` unset = `count`) to a half-open zone.
pub fn resolve_zone(
    count: FrameId,
    loop_in: FrameId,
    loop_out: Option<FrameId>,
) -> (FrameId, FrameId) {
    let z = LoopZone::normalize(loop_in, loop_out.unwrap_or(count), count);
    (z.in_frame, z.out_frame)
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zone_owner_from_spans_table() {
        // Zone ⊆ live [0, 200) → Engine.
        assert_eq!(
            zone_owner_from_spans(true, 10, 100, 0, 200),
            ZoneOwner::Engine
        );
        // Cross-clip / wider than live → Host.
        assert_eq!(zone_owner_from_spans(true, 0, 500, 0, 200), ZoneOwner::Host);
        assert_eq!(
            zone_owner_from_spans(false, 10, 100, 0, 200),
            ZoneOwner::Off
        );
        // Empty / inverted zone → Host (not Engine).
        assert_eq!(zone_owner_from_spans(true, 50, 50, 0, 200), ZoneOwner::Host);
    }

    #[test]
    fn seam_phase_transitions() {
        assert_eq!(seam_try_begin(SeamPhase::Idle), Some(SeamPhase::Preparing));
        assert_eq!(seam_try_begin(SeamPhase::Preparing), None);
        assert_eq!(seam_try_begin(SeamPhase::Swapping), None);
        assert_eq!(seam_try_begin(SeamPhase::Playing), None);

        assert_eq!(
            seam_ensure_preparing(SeamPhase::Idle),
            Some(SeamPhase::Preparing)
        );
        assert_eq!(
            seam_ensure_preparing(SeamPhase::Preparing),
            Some(SeamPhase::Preparing)
        );
        assert_eq!(seam_ensure_preparing(SeamPhase::Swapping), None);
        assert_eq!(seam_ensure_preparing(SeamPhase::Playing), None);

        assert!(!SeamPhase::Idle.is_busy());
        assert!(SeamPhase::Preparing.is_busy());
        assert!(SeamPhase::Swapping.is_busy());
        assert!(SeamPhase::Playing.is_busy());
    }

    #[test]
    fn play_outside_zone_table() {
        let host = ZoneOwner::Host;
        assert!(play_outside_zone(host, true, 100, 500, 1000, 50));
        assert!(play_outside_zone(host, true, 100, 500, 1000, 499));
        assert!(!play_outside_zone(host, true, 100, 500, 1000, 100));
        assert!(!play_outside_zone(host, true, 100, 500, 1000, 250));
        assert!(!play_outside_zone(host, true, 100, 500, 1000, 498));
        assert!(!play_outside_zone(host, true, 0, 1000, 1000, 0));
        assert!(!play_outside_zone(
            ZoneOwner::Engine,
            true,
            100,
            500,
            1000,
            50
        ));
        assert!(!play_outside_zone(host, false, 100, 500, 1000, 50));
    }

    fn scrub_host(global: u32, resume: bool, on_live: bool, seam_busy: bool) -> ScrubUpCtx {
        ScrubUpCtx {
            global,
            resume,
            aim_on_live: on_live,
            owner: ZoneOwner::Host,
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
            plan_scrub_up(scrub_host(50, true, true, false)),
            ScrubUpPlan::HostWrap {
                seam_already_busy: false
            }
        );
        assert_eq!(
            plan_scrub_up(scrub_host(50, true, false, true)),
            ScrubUpPlan::HostWrap {
                seam_already_busy: true
            }
        );
        assert!(!matches!(
            plan_scrub_up(scrub_host(50, true, true, false)),
            ScrubUpPlan::LiveSettle { .. } | ScrubUpPlan::CrossClipSeek { .. }
        ));
    }

    #[test]
    fn scrub_up_paused_outside_settles_normally() {
        assert_eq!(
            plan_scrub_up(scrub_host(50, false, true, false)),
            ScrubUpPlan::LiveSettle { then_play: false }
        );
        assert_eq!(
            plan_scrub_up(scrub_host(50, false, false, false)),
            ScrubUpPlan::CrossClipSeek { then_play: false }
        );
    }

    #[test]
    fn scrub_up_resume_inside_plays_via_settle_or_seek() {
        assert_eq!(
            plan_scrub_up(scrub_host(250, true, true, false)),
            ScrubUpPlan::LiveSettle { then_play: true }
        );
        assert_eq!(
            plan_scrub_up(scrub_host(250, true, false, false)),
            ScrubUpPlan::CrossClipSeek { then_play: true }
        );
    }

    #[test]
    fn scrub_up_engine_owner_outside_does_not_host_wrap() {
        let mut ctx = scrub_host(50, true, true, false);
        ctx.owner = ZoneOwner::Engine;
        assert_eq!(
            plan_scrub_up(ctx),
            ScrubUpPlan::LiveSettle { then_play: true }
        );
    }

    fn outside_ctx(global: u32, wall_on_live: bool, need_wall: bool) -> HostOutsideCtx {
        HostOutsideCtx {
            owner: ZoneOwner::Host,
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
        eng.owner = ZoneOwner::Engine;
        assert_eq!(plan_host_outside(eng), HostOutsidePlan::EngineSlotPlay);
        assert_eq!(
            plan_host_outside(outside_ctx(250, false, false)),
            HostOutsidePlan::NotOutside
        );
    }

    #[test]
    fn wall_effect_past_out_requires_wall_on_live() {
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
        assert_eq!(
            plan_host_outside(outside_ctx(50, false, true)),
            HostOutsidePlan::HostWrap {
                seam_already_busy: false
            }
        );
    }

    #[test]
    fn soft_seam_cancel_strings() {
        assert!(is_soft_seam_cancel("operation cancelled"));
        assert!(is_soft_seam_cancel("cancelled"));
        assert!(is_soft_seam_cancel("Cancelled"));
        assert!(!is_soft_seam_cancel("open timed out"));
        assert!(!is_soft_seam_cancel("LoopZone in out of range"));
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

    #[test]
    fn seam_next_in_zone_vs_wrap_to_in_clip() {
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
            owner: ZoneOwner::Host,
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
        assert_eq!(
            plan_advance_ended(advance_host(Some(1), 150, 200)),
            AdvanceEndedPlan::Wrap
        );
        assert_eq!(
            plan_advance_ended(AdvanceEndedCtx {
                zone_in_aim: Some((0, 0)),
                ..advance_host(Some(1), 500, 200)
            }),
            AdvanceEndedPlan::Promote { clip_index: 1 }
        );
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
                owner: ZoneOwner::Host,
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

#[cfg(test)]
mod zone_store_tests {
    use super::*;

    #[test]
    fn store_zone_full_axis_leaves_out_unset() {
        assert_eq!(store_zone(10, 0, 10).loop_out, None);
        assert_eq!(store_zone(10, 0, 99).loop_out, None);
        assert_eq!(store_zone(0, 3, 5).loop_out, None);
        assert_eq!(store_zone(10, 2, 10).loop_out, Some(10));
        assert_eq!(store_zone(10, 0, 4).loop_out, Some(4));
        assert_eq!(store_zone(10, 7, 7).loop_out, Some(8));
    }

    #[test]
    fn resolve_zone_defaults_out_to_count() {
        assert_eq!(resolve_zone(10, 0, None), (0, 10));
        assert_eq!(resolve_zone(10, 3, Some(6)), (3, 6));
        assert_eq!(resolve_zone(0, 3, None), (0, 0));
        assert_eq!(resolve_zone(1, 0, None), (0, 1));
    }
}
