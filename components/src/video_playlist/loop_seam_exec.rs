//! Host LoopSeam adapter — wrap / promote / EOF advance exec.
//!
//! Pure latch + owner law: [`super::loop_seam`] / VT `loop_zone`.
//! Slot open/swap/warm primitives: [`super::slots`].
//! Engine intra-clip loop stays live `VideoPlayer` `LoopSeam`.
//! Design: `.ai/architecture.md` § LoopSeam vs host promote.
//! **no equivalent** (playlist local).

use leptos::prelude::*;

use crate::frame_axis::{ClipFrame, GlobalFrame};
use crate::video_playlist::PlaylistPos;

use super::loop_seam::{
    LoopOwner, SeamPhase, end_seam, ensure_seam_preparing, loop_owner, seam_busy, seam_trace,
    set_seam_phase, try_begin_seam,
};
use super::plan::{
    AdvanceEndedCtx, AdvanceEndedPlan, WrapDonePlan, plan_advance_ended, plan_wrap_done,
};
use super::slots::{
    PromoteLead, ensure_clip_on_idle, idle_neighbor_for_zone, preload_idle,
    resume_live_if_playlist_playing, seam_next_clip, stop_live_for_host_seam, swap_live_idle,
    sync_live_loop_from_playlist,
};
use super::video_playlist_ref::VideoPlaylistRef;

/// Snap playlist global aim to `zone_in` so wall cannot re-fire on stale `g`.
fn reassert_zone_in_playhead(pl: VideoPlaylistRef) {
    let (zone_in, _) = pl.loop_zone();
    let model = pl.model.get_untracked();
    let Some(aim) = model.at_global_frame(GlobalFrame::new(zone_in)) else {
        return;
    };
    pl.pos.set(aim);
    if let Some(g) = model.global_frame_at(aim).map(GlobalFrame::get) {
        pl.playhead.set(g);
    }
}

/// Idle Play-ready (`SeamHold`) → swap → PlayPrime release. Shared by clip cut
/// (`Origin`) and cross-clip LoopZone wrap (`Mid`). Arm on idle main — no
/// post-swap `warm_play_origin`. `expect_live`: abort if live moved (EOF race).
/// **no equivalent** (playlist host).
async fn promote_idle_at(
    pl: VideoPlaylistRef,
    epoch: u64,
    clip_index: u32,
    local_frame: u32,
    expect_live: Option<u32>,
) -> Result<(), String> {
    let lead = PromoteLead::for_local(local_frame);
    let local = lead.local();
    ensure_clip_on_idle(pl, clip_index, epoch).await?;
    if pl.open_gen.get_value() != epoch {
        return Ok(());
    }
    if expect_live.is_some_and(|from| pl.live_clip.get_untracked() != from) {
        return Ok(());
    }
    // Stop live **before** idle prepare (not only at swap) — Host wall overrun.
    stop_live_for_host_seam(pl);
    lead.prepare(pl.idle_video()).await?;
    if pl.open_gen.get_value() != epoch {
        // New open supersedes — do not resume stale slot.
        return Ok(());
    }
    if expect_live.is_some_and(|from| pl.live_clip.get_untracked() != from) {
        resume_live_if_playlist_playing(pl);
        return Ok(());
    }
    // swap_live_idle also hard-stops outgoing if still marked playing.
    // Keep playlist `playing` intent true across hard-stop → PlayPrime gap.
    seam_trace(&format!(
        "promote swap→play clip={clip_index} local={local} lead={lead:?}"
    ));
    set_seam_phase(pl, SeamPhase::Swapping);
    swap_live_idle(pl, clip_index);
    pl.seam_warm_for.set_value(None);
    // Facade must match new live — else mirror `pos.clip ≠ live` skip (tip-jump
    // guard) freezes the progress bar while Play continues. **no equivalent**.
    let facade = PlaylistPos::new(clip_index, ClipFrame::from_source(local));
    pl.pos.set(facade);
    if let Some(g) = pl
        .model
        .get_untracked()
        .global_frame_at(facade)
        .map(GlobalFrame::get)
    {
        pl.playhead.set(g);
    }
    let live = pl.live_video();
    live.ended.set(false);
    if live.playhead.get_untracked() != local {
        live.playhead.set(local);
    }
    set_seam_phase(pl, SeamPhase::Playing);
    pl.playing.set(true);
    // Host mid-zone: release restarts PlayIter (prepare-only refresh re-snapped
    // to loop_in). Full-axis keeps hold iter. **no equivalent** (playlist local).
    let (zone_in, zone_out) = pl.loop_zone();
    let total = pl.model.get_untracked().total_frame_count();
    let zone_full = zone_in == 0 && zone_out == total;
    let restart =
        pl.looping.get_untracked() && matches!(loop_owner(pl), LoopOwner::Host) && !zone_full;
    live.set_seam_hold_restart_on_release(restart);
    live.play();
    Ok(())
}

/// Host LoopZone wrap → `promote_idle_at` at `zone_in` (or same-clip paint).
/// Used when [`LoopOwner::Host`]. Intra-clip → engine `LoopSeam` instead.
/// **no equivalent** (playlist local).
pub(crate) async fn wrap_to_loop_in(pl: VideoPlaylistRef, epoch: u64) -> Result<(), String> {
    if pl.open_gen.get_value() != epoch {
        end_seam(pl);
        return Ok(());
    }
    if !ensure_seam_preparing(pl) {
        // Another seam owns Swapping/Playing — do not steal / silent-stop.
        seam_trace("wrap skip ensure_seam_preparing");
        return Ok(());
    }
    // Freeze live immediately so Play cannot overrun `zone_last` during prepare
    // (mirror gated → scrubber stuck while canvas runs past the loop).
    stop_live_for_host_seam(pl);
    let (zone_in_log, _) = pl.loop_zone();
    seam_trace(&format!(
        "wrap enter epoch={epoch} zone_in={zone_in_log} live_clip={}",
        pl.live_clip.get_untracked()
    ));
    let mut neighbor_after: Option<u32> = None;
    let result: Result<(), String> = async {
        let (zone_in, zone_out) = pl.loop_zone();
        let model = pl.model.get_untracked();
        let Some(aim) = model.at_global_frame(GlobalFrame::new(zone_in)) else {
            resume_live_if_playlist_playing(pl);
            return Err("LoopZone in out of range".into());
        };
        pl.status.set(format!("LoopZone → frame {zone_in}…"));
        pl.seam_warm_for.set_value(None);
        pl.ended.set(false);
        if let Some(g) = model.global_frame_at(aim).map(GlobalFrame::get) {
            pl.playhead.set(g);
        }
        pl.pos.set(aim);

        // Same-clip wrap (fallback): paint + play. Prefer engine LoopSeam via
        // `sync_live_loop_from_playlist` — never leave `set_loop(false)` here
        // (that re-broke intra-clip sharing from `aa8ac99` / `2c19b70`).
        if pl.live_clip.get_untracked() == aim.clip_index {
            seam_trace(&format!(
                "wrap same-clip prepare local={}",
                aim.frame.source()
            ));
            let lead = PromoteLead::for_local(aim.frame.source());
            let live = pl.live_video();
            live.ended.set(false);
            lead.prepare(live).await?;
            if pl.open_gen.get_value() != epoch {
                return Ok(());
            }
            live.playhead.set(lead.local());
            sync_live_loop_from_playlist(pl);
            set_seam_phase(pl, SeamPhase::Playing);
            pl.playing.set(true);
            let total = model.total_frame_count();
            let zone_full = zone_in == 0 && zone_out == total;
            let restart = pl.looping.get_untracked()
                && matches!(loop_owner(pl), LoopOwner::Host)
                && !zone_full;
            live.set_seam_hold_restart_on_release(restart);
            live.play();
            return Ok(());
        }

        seam_trace(&format!(
            "wrap promote clip={} local={}",
            aim.clip_index,
            aim.frame.source()
        ));
        promote_idle_at(pl, epoch, aim.clip_index, aim.frame.source(), None).await?;
        if pl.open_gen.get_value() != epoch {
            return Ok(());
        }
        // Defer neighbor open until after `end_seam` (mirror). **no equivalent**.
        neighbor_after = idle_neighbor_for_zone(
            &model,
            aim.clip_index,
            pl.looping.get_untracked(),
            zone_in,
            zone_out,
        );
        Ok(())
    }
    .await;
    // Soft vs hard classified by [`plan_wrap_done`] at the call boundary — do
    // not swallow Cancelled here (regression: sticky UI error vs silent Ok).
    // Belt: mirror was gated during busy; re-snap before Idle so wall cannot
    // re-fire on a stale g >= zone_last while PlayPrime is still queued.
    reassert_zone_in_playhead(pl);
    seam_trace(&format!(
        "wrap done ok={} playhead={}",
        result.is_ok(),
        pl.playhead.get_untracked()
    ));
    end_seam(pl);
    if result.is_ok() {
        preload_idle(pl, neighbor_after, epoch).await;
    }
    result
}

/// Begin Host LoopZone wrap (PlayPressed / wall Effect / scrub Up after preempt).
/// Named adapter entry for Host wall — Engine owner uses live `LoopSeam`.
/// When seam busy: keep facade Play so Effect can retry — do not sticky-error.
pub(crate) fn start_host_wrap(pl: VideoPlaylistRef, seam_already_busy: bool) {
    if seam_already_busy || !try_begin_seam(pl) {
        pl.playing.set(true);
        return;
    }
    pl.playing.set(true);
    let epoch = pl.open_gen.get_value();
    wasm_bindgen_futures::spawn_local(async move {
        let result = wrap_to_loop_in(pl, epoch).await;
        run_wrap_done(pl, epoch, result, "LoopZone wrap failed.");
    });
}

/// Apply [`plan_wrap_done`] after Host wrap / seam async (soft ≠ sticky error).
pub(crate) fn run_wrap_done(
    pl: VideoPlaylistRef,
    epoch: u64,
    result: Result<(), String>,
    status: &str,
) {
    let epoch_ok = pl.open_gen.get_value() == epoch;
    let plan = match &result {
        Ok(()) => plan_wrap_done(Ok(()), epoch_ok),
        Err(e) => plan_wrap_done(Err(e.as_str()), epoch_ok),
    };
    match plan {
        WrapDonePlan::Ok | WrapDonePlan::Ignore => {}
        WrapDonePlan::ResumeLiveKeepPlay => {
            seam_trace("wrap_done soft-cancel → resume");
            resume_live_if_playlist_playing(pl);
        }
        WrapDonePlan::HardFail => {
            let Err(err) = result else {
                return;
            };
            seam_trace(&format!("wrap_done hard: {err}"));
            pl.playing.set(false);
            pl.error.set(Some(err));
            pl.status.set(status.into());
        }
    }
}

/// Host seam / wrap failed after live was already stopped — [`run_wrap_done`].
pub(crate) fn note_host_seam_err(pl: VideoPlaylistRef, epoch: u64, err: String, status: &str) {
    run_wrap_done(pl, epoch, Err(err), status);
}

/// After live clip EOF: [`plan_advance_ended`] → promote / wrap / terminal.
/// `from_clip` = ended clip (ignore if live already moved).
/// Do **not** bail on sticky `playing` after Effect cleared `ended` — **no equivalent**.
pub(crate) async fn advance_after_ended(
    pl: VideoPlaylistRef,
    epoch: u64,
    from_clip: u32,
) -> Result<(), String> {
    if pl.open_gen.get_value() != epoch {
        end_seam(pl);
        return Ok(());
    }
    if !ensure_seam_preparing(pl) {
        return Ok(());
    }
    let result = async {
        let model = pl.model.get_untracked();
        let (zone_in, zone_out) = pl.loop_zone();
        let total = model.total_frame_count();
        let zone_full = zone_in == 0 && zone_out == total;
        let zone_in_aim = model
            .at_global_frame(GlobalFrame::new(zone_in))
            .map(|p| (p.clip_index, p.frame.source()));
        let seam_next = seam_next_clip(pl, from_clip);
        let plan = plan_advance_ended(AdvanceEndedCtx {
            live_still_from_clip: pl.live_clip.get_untracked() == from_clip,
            owner: loop_owner(pl),
            zone_full,
            zone_out,
            clip_end: model.clip_end_frame(from_clip).map(GlobalFrame::get).unwrap_or(0),
            zone_in_aim,
            seam_next,
            looping: pl.looping.get_untracked(),
        });
        seam_trace(&format!(
            "advance plan={plan:?} from_clip={from_clip} zone_in_aim={zone_in_aim:?} seam_next={seam_next:?} zone_out={zone_out}"
        ));
        match plan {
            AdvanceEndedPlan::Noop => Ok(()),
            AdvanceEndedPlan::Wrap => wrap_to_loop_in(pl, epoch).await,
            AdvanceEndedPlan::Promote { clip_index: next_i } => {
                pl.status.set(format!("Seam → clip {}…", next_i + 1));
                promote_idle_at(pl, epoch, next_i, 0, Some(from_clip)).await?;
                if pl.open_gen.get_value() != epoch {
                    return Ok(());
                }
                if pl.live_clip.get_untracked() != next_i {
                    return Err(format!("seam promote did not land on clip {}", next_i + 1));
                }
                pl.ended.set(false);
                pl.status
                    .set(format!("Playing clip {}/{}", next_i + 1, model.len()));
                // Unblock facade mirror before neighbor open — awaiting preload
                // under seam_busy froze progress/frame on 3-clip first join while
                // SeamHold canvas already played. **no equivalent**.
                end_seam(pl);
                let following = idle_neighbor_for_zone(
                    &model,
                    next_i,
                    pl.looping.get_untracked(),
                    zone_in,
                    zone_out,
                );
                preload_idle(pl, following, epoch).await;
                Ok(())
            }
            AdvanceEndedPlan::TerminalEnd => {
                pl.ended.set(true);
                pl.playing.set(false);
                pl.status.set("Ended.".into());
                Ok(())
            }
        }
    }
    .await;
    // Nested wrap_to_loop_in already ended the seam; only clear if still busy.
    if seam_busy(pl) {
        end_seam(pl);
    }
    result
}
