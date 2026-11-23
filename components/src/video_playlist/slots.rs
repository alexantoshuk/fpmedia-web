//! Live/idle slot helpers for [`super::VideoPlaylist`].
//!
//! Host LoopZone wrap / promote / EOF advance: [`super::loop_seam_exec`].

use fpmedia_web::timer::sleep_ms;
use fpmedia_web::{
    LOOP_ORIGIN_PREWARM_SECS, MediaError, ScrubCachePolicy, VideoMetadata,
    should_prewarm_loop_origin,
};
use leptos::prelude::*;

use crate::VideoRef;
use crate::frame_axis::GlobalFrame;
use crate::local_epoch::LocalEpoch;
use crate::video_playlist::PlaylistModel;
use fpmedia_web::Ticket;

use super::host_zone::is_soft_seam_cancel;
use super::loop_seam::{LoopOwner, loop_owner, seam_busy, seam_trace};
use super::plan::{SeamNextCtx, plan_seam_next_clip};
use super::video_playlist_ref::VideoPlaylistRef;

pub(crate) const PROBE_TIMEOUT_MS: f64 = 120_000.0;
pub(crate) const PROBE_POLL_MS: i32 = 32;

pub(crate) async fn wait_ready(
    video: VideoRef,
    epoch: Ticket,
    open_gen: LocalEpoch,
) -> Result<VideoMetadata, MediaError> {
    let start = fpmedia_web::timer::now_ms().unwrap_or(0.0);
    loop {
        if !open_gen.is_current(epoch) {
            return Err(MediaError::Cancelled);
        }
        if let Some(err) = video.error.get_untracked() {
            return Err(MediaError::Source(err));
        }
        if video.ready.get_untracked()
            && let Some(meta) = video.metadata.get_untracked()
        {
            return Ok(meta);
        }
        let now = fpmedia_web::timer::now_ms().unwrap_or(start);
        if now - start > PROBE_TIMEOUT_MS {
            return Err(MediaError::Source("open timed out".into()));
        }
        sleep_ms(PROBE_POLL_MS).await;
    }
}

/// Prefer next clip as idle neighbor; else wrap target 0 when playlist looping,
/// else previous. Wrap → idle preload 0 so origin stash warms before last→0 seam.
/// **no equivalent** (playlist local).
pub(crate) fn idle_neighbor(model: &PlaylistModel, live_clip: u32, looping: bool) -> Option<u32> {
    model.next_clip_index(live_clip).or_else(|| {
        if looping && model.len() > 1 {
            Some(0)
        } else {
            live_clip.checked_sub(1)
        }
    })
}

/// Open `want` on idle when missing (coalesced). Scrub cut-proximity warm.
pub(crate) fn kick_ensure_idle(pl: VideoPlaylistRef, want: u32) {
    if pl.idle_clip.get_untracked() == Some(want) && pl.idle_video().ready.get_untracked() {
        kick_cut_join_exact_warm(pl);
        return;
    }
    if pl.latches.get_value().ensure_idle_for == Some(want) {
        return;
    }
    let epoch = pl.open_gen.ticket();
    wasm_bindgen_futures::spawn_local(async move {
        let _ = ensure_clip_on_idle(pl, want, epoch).await;
        kick_cut_join_exact_warm(pl);
    });
}

/// Pause-only one-shot Exact@cut on idle so cross-clip Up can PresentHit.
///
/// On → densify-free [`VideoRef::prepare_seam_at`] (Exact ingest) → park (keep
/// Exact). Kick-once per `(idle_clip, join)`; not every Move; not soft-drag;
/// not Play SeamHold. **no equivalent** (playlist scrub local).
pub(crate) fn kick_cut_join_exact_warm(pl: VideoPlaylistRef) {
    if pl.playing.get_untracked() || pl.scrubbing.get_untracked() {
        return;
    }
    let Some(idle_i) = pl.idle_clip.get_untracked() else {
        return;
    };
    let live_i = pl.live_clip.get_untracked();
    let Some(join) =
        pl.model
            .get_untracked()
            .cut_join_local(live_i, idle_i, pl.looping.get_untracked())
    else {
        return;
    };
    let idle = pl.idle_video();
    if !idle.ready.get_untracked() || idle.has_scrub_exact_blit(join) {
        return;
    }
    if pl.latches.get_value().cut_join_warm_for == Some((idle_i, join)) {
        return;
    }
    pl.latches
        .update_value(|l| l.cut_join_warm_for = Some((idle_i, join)));
    let epoch = pl.open_gen.ticket();
    wasm_bindgen_futures::spawn_local(async move {
        let _ = warm_cut_join_exact(pl, idle_i, join, epoch).await;
        if pl.latches.get_value().cut_join_warm_for == Some((idle_i, join)) {
            pl.latches.update_value(|l| l.cut_join_warm_for = None);
        }
    });
}

async fn warm_cut_join_exact(
    pl: VideoPlaylistRef,
    idle_i: u32,
    join: u32,
    epoch: Ticket,
) -> Result<(), MediaError> {
    if !pl.open_gen.is_current(epoch) {
        return Ok(());
    }
    if pl.playing.get_untracked() || pl.scrubbing.get_untracked() {
        return Ok(());
    }
    if pl.idle_clip.get_untracked() != Some(idle_i) {
        return Ok(());
    }
    let opening_is_a = !pl.live_is_a.get_untracked();
    let idle = slot_is_a(pl, opening_is_a);
    if !opening_still_idle(pl, opening_is_a) {
        return Ok(());
    }
    if !idle.ready.get_untracked() || idle.has_scrub_exact_blit(join) {
        return Ok(());
    }
    // Off parks block Exact ingest — On for one densify-free Exact, then park.
    idle.set_scrub_cache_policy(ScrubCachePolicy::On);
    let prepared = idle.prepare_seam_at(join).await;
    if let Some(r) = finish_if_opening_became_live(pl, opening_is_a, idle_i) {
        return r;
    }
    if ensure_live_on(pl, idle_i) {
        pl.apply_gain_to_slots();
        return Ok(());
    }
    if pl.idle_clip.get_untracked() == Some(idle_i) && opening_still_idle(pl, opening_is_a) {
        idle.park_scrub_cache();
    }
    pl.apply_gain_to_slots();
    prepared
}

/// Idle neighbor for LoopZone: sequential next while it starts before `zone_out`,
/// else the clip that owns `zone_in` (wrap target). **no equivalent**.
pub(crate) fn idle_neighbor_for_zone(
    model: &PlaylistModel,
    live_clip: u32,
    looping: bool,
    zone_in: u32,
    zone_out: u32,
) -> Option<u32> {
    if !looping {
        return idle_neighbor(model, live_clip, false);
    }
    if let Some(next) = model.next_clip_index(live_clip) {
        let next_start = model
            .clip_start_frame(next)
            .map(GlobalFrame::get)
            .unwrap_or(0);
        if next_start < zone_out {
            return Some(next);
        }
    }
    model
        .at_global_frame(GlobalFrame::new(zone_in))
        .map(|p| p.clip_index)
        .or_else(|| idle_neighbor(model, live_clip, true))
}

/// Slot we captured as idle (`opening_is_a`) is still idle iff live flipped away.
fn opening_still_idle(pl: VideoPlaylistRef, opening_is_a: bool) -> bool {
    pl.live_is_a.get_untracked() != opening_is_a
}

fn slot_is_a(pl: VideoPlaylistRef, is_a: bool) -> VideoRef {
    if is_a { pl.slot_a } else { pl.slot_b }
}

/// After await: promote may have made our opening slot live — On + bail, never park/close.
/// **no equivalent** (playlist scrub local).
fn finish_if_opening_became_live(
    pl: VideoPlaylistRef,
    opening_is_a: bool,
    clip_index: u32,
) -> Option<Result<(), MediaError>> {
    if opening_still_idle(pl, opening_is_a) {
        return None;
    }
    // Captured slot is now live.
    if pl.live_clip.get_untracked() == clip_index {
        let live = pl.live_video();
        live.set_scrub_cache_policy(ScrubCachePolicy::On);
        pl.apply_gain_to_slots();
        return Some(Ok(()));
    }
    Some(Err(MediaError::Cancelled))
}

fn ensure_live_on(pl: VideoPlaylistRef, clip_index: u32) -> bool {
    if pl.live_clip.get_untracked() != clip_index {
        return false;
    }
    let live = pl.live_video();
    live.set_scrub_cache_policy(ScrubCachePolicy::On);
    true
}

pub(crate) async fn ensure_clip_on_idle(
    pl: VideoPlaylistRef,
    clip_index: u32,
    epoch: Ticket,
) -> Result<(), MediaError> {
    if !pl.open_gen.is_current(epoch) {
        return Err(MediaError::Cancelled);
    }
    if ensure_live_on(pl, clip_index) {
        pl.apply_gain_to_slots();
        return Ok(());
    }
    if pl.idle_clip.get_untracked() == Some(clip_index) && pl.idle_video().ready.get_untracked() {
        kick_cut_join_exact_warm(pl);
        return Ok(());
    }
    // Coalesce Move stampede (2→1 reverse cut) — in-flight same clip: wait for ready.
    if pl.latches.get_value().ensure_idle_for == Some(clip_index) {
        loop {
            if !pl.open_gen.is_current(epoch) {
                return Err(MediaError::Cancelled);
            }
            if ensure_live_on(pl, clip_index) {
                pl.apply_gain_to_slots();
                return Ok(());
            }
            if pl.idle_clip.get_untracked() == Some(clip_index)
                && pl.idle_video().ready.get_untracked()
            {
                kick_cut_join_exact_warm(pl);
                return Ok(());
            }
            if pl.latches.get_value().ensure_idle_for != Some(clip_index) {
                break;
            }
            sleep_ms(PROBE_POLL_MS).await;
        }
        if ensure_live_on(pl, clip_index) {
            pl.apply_gain_to_slots();
            return Ok(());
        }
        if pl.idle_clip.get_untracked() == Some(clip_index) && pl.idle_video().ready.get_untracked()
        {
            kick_cut_join_exact_warm(pl);
            return Ok(());
        }
    }
    let src = pl
        .sources
        .try_with_value(|s| s.get(clip_index as usize).cloned())
        .flatten()
        .ok_or_else(|| MediaError::Source(format!("missing source for clip {}", clip_index + 1)))?;
    let opening_is_a = !pl.live_is_a.get_untracked();
    let idle = slot_is_a(pl, opening_is_a);
    pl.latches
        .update_value(|l| l.ensure_idle_for = Some(clip_index));
    idle.set_loop(false);
    // Teardown wipe old media Exact; after open park (densify off, no wipe).
    // Promote → On. Stale task must not close/park after promote. **no equivalent**.
    idle.set_scrub_cache_policy(ScrubCachePolicy::Off);
    idle.close();
    sleep_ms(0).await;
    let clear_flight = |pl: VideoPlaylistRef, want: u32| {
        if pl.latches.get_value().ensure_idle_for == Some(want) {
            pl.latches.update_value(|l| l.ensure_idle_for = None);
        }
    };
    if !pl.open_gen.is_current(epoch) {
        clear_flight(pl, clip_index);
        return Err(MediaError::Cancelled);
    }
    if let Some(r) = finish_if_opening_became_live(pl, opening_is_a, clip_index) {
        clear_flight(pl, clip_index);
        return r;
    }
    if ensure_live_on(pl, clip_index) {
        clear_flight(pl, clip_index);
        pl.apply_gain_to_slots();
        return Ok(());
    }
    if !opening_still_idle(pl, opening_is_a) {
        clear_flight(pl, clip_index);
        return Err(MediaError::Cancelled);
    }
    src.open_on(idle);
    pl.idle_clip.set(Some(clip_index));
    let ready = wait_ready(idle, epoch, pl.open_gen).await;
    if let Some(r) = finish_if_opening_became_live(pl, opening_is_a, clip_index) {
        clear_flight(pl, clip_index);
        return r;
    }
    if let Err(e) = ready {
        clear_flight(pl, clip_index);
        return Err(e);
    }
    // Move may promote during wait_ready sleep — never park a live slot
    // (Host Off sticks; Exact ingest dies). **no equivalent**.
    if ensure_live_on(pl, clip_index) {
        // live may be the captured slot (promote) or the other — On via live_video.
    } else if pl.idle_clip.get_untracked() == Some(clip_index)
        && opening_still_idle(pl, opening_is_a)
    {
        idle.park_scrub_cache();
    }
    clear_flight(pl, clip_index);
    // Open resets engine A/V — re-apply playlist-wide volume/mute/rate.
    pl.apply_gain_to_slots();
    kick_cut_join_exact_warm(pl);
    Ok(())
}

/// Map playlist global LoopZone onto the live slot from [`loop_owner`].
///
/// `LoopOwner::Engine` → live `set_loop_zone` + `set_loop(true)` (engine `LoopSeam`).
/// `Host` / `Off` → `live.set_loop(false)`; host wrap via [`promote_idle_at`].
/// Idle never engine-loops. **no equivalent** (playlist mapping local).
pub(crate) fn sync_live_loop_from_playlist(pl: VideoPlaylistRef) {
    let live = pl.live_video();
    let idle = pl.idle_video();
    idle.set_loop(false);

    match loop_owner(pl) {
        LoopOwner::Off | LoopOwner::Host => {
            live.set_loop(false);
        }
        LoopOwner::Engine => {
            let model = pl.model.get_untracked();
            let live_i = pl.live_clip.get_untracked();
            let (zone_in, zone_out) = pl.loop_zone();
            let Some(clip_start) = model.clip_start_frame(live_i).map(GlobalFrame::get) else {
                live.set_loop(false);
                return;
            };
            let local_in = zone_in - clip_start;
            let local_out = zone_out - clip_start;
            live.set_loop_zone(local_in, local_out);
            live.set_loop(true);
        }
    }
}

/// Flip live/idle slots. `idle_clip` becomes the clip that **physically** moved
/// to idle (former live) — never an aspirational neighbor (that lied and made
/// scrub promote the wrong media). Caller [`preload_idle`]s the desired neighbor.
///
/// Policy On = scrub-cache **capability** only (does not arm densify).
/// Live On; idle **park** (densify off, keep Exact). Off wipe only on media
/// replace / user Scrub Off. Exact = slot blit. **no equivalent**.
pub(crate) fn swap_live_idle(pl: VideoPlaylistRef, new_live_clip: u32) {
    // Stop outgoing before flip — else hidden slot keeps Playing and can EOF /
    // densify while the new live seeks (LoopZone wrap flash).
    let outgoing = pl.live_video();
    if outgoing.playing.get_untracked() || outgoing.ended.get_untracked() {
        outgoing.pause_hard_no_densify();
    }
    let former_live = pl.live_clip.get_untracked();
    pl.live_is_a.set(!pl.live_is_a.get_untracked());
    pl.live_clip.set(new_live_clip);
    pl.idle_clip.set(Some(former_live));
    let live = pl.live_video();
    live.set_scrub_cache_policy(ScrubCachePolicy::On);
    live.ended.set(false);
    let idle = pl.idle_video();
    idle.park_scrub_cache();
    idle.ended.set(false);
    sync_live_loop_from_playlist(pl);
    pl.apply_gain_to_slots();
}

/// Sync promote when idle already holds `clip_index` and is ready.
pub(crate) fn try_promote_idle(pl: VideoPlaylistRef, clip_index: u32) -> bool {
    if pl.live_clip.get_untracked() == clip_index {
        return true;
    }
    if pl.idle_clip.get_untracked() != Some(clip_index) || !pl.idle_video().ready.get_untracked() {
        return false;
    }
    swap_live_idle(pl, clip_index);
    true
}

pub(crate) async fn preload_idle(pl: VideoPlaylistRef, clip_index: Option<u32>, epoch: Ticket) {
    let Some(i) = clip_index else {
        return;
    };
    if !pl.open_gen.is_current(epoch) {
        return;
    }
    if ensure_live_on(pl, i) {
        pl.apply_gain_to_slots();
        return;
    }
    if pl.idle_clip.get_untracked() == Some(i) && pl.idle_video().ready.get_untracked() {
        return;
    }
    let Some(src) = pl
        .sources
        .try_with_value(|s| s.get(i as usize).cloned())
        .flatten()
    else {
        return;
    };
    let opening_is_a = !pl.live_is_a.get_untracked();
    let idle = slot_is_a(pl, opening_is_a);
    idle.set_loop(false);
    // Replace media → wipe; after open park unless promote won the race.
    // Stale preload must not Off/park a promoted live. **no equivalent**.
    idle.set_scrub_cache_policy(ScrubCachePolicy::Off);
    src.open_on(idle);
    pl.idle_clip.set(Some(i));
    let _ = wait_ready(idle, epoch, pl.open_gen).await;
    if finish_if_opening_became_live(pl, opening_is_a, i).is_some() {
        return;
    }
    if ensure_live_on(pl, i) {
        pl.apply_gain_to_slots();
        return;
    }
    if pl.idle_clip.get_untracked() == Some(i) && opening_still_idle(pl, opening_is_a) {
        idle.park_scrub_cache();
    }
    pl.apply_gain_to_slots();
    kick_cut_join_exact_warm(pl);
}

/// Make `aim.clip_index` live (swap or open) at `aim.frame.source()`.
///
/// Cross-clip: paint on idle → swap. `then_play`: SeamHold (`PromoteLead`) →
/// `play()`; pause/goto: slot Exact PresentHit or seek densify.
/// Same-clip pause: same law. **no equivalent**.
pub(crate) async fn seek_to_pos(
    pl: VideoPlaylistRef,
    aim: crate::video_playlist::PlaylistPos,
    epoch: Ticket,
    then_play: bool,
) -> Result<(), MediaError> {
    if !pl.open_gen.is_current(epoch) {
        return Ok(());
    }
    pl.ended.set(false);
    pl.pos.set(aim);
    let global = pl
        .model
        .get_untracked()
        .global_frame_at(aim)
        .map(GlobalFrame::get)
        .unwrap_or(0);
    pl.playhead.set(global);
    let _ = global; // playhead already set above

    if pl.live_clip.get_untracked() != aim.clip_index {
        ensure_clip_on_idle(pl, aim.clip_index, epoch).await?;
        if !pl.open_gen.is_current(epoch) {
            return Ok(());
        }
        // Stop outgoing Play before idle Exact / SeamHold (goto / scrub Up).
        stop_live_for_host_seam(pl);
        let idle = pl.idle_video();
        let slot_exact_hit = !then_play && idle.has_scrub_exact_blit(aim.frame.source());
        if then_play {
            // Play join: idle main SeamHold → promote → PlayPrime release.
            PromoteLead::for_local(aim.frame.source())
                .prepare(idle)
                .await?;
        } else if !slot_exact_hit {
            // Paused miss: On so prepare Exact ingest works (Off gates blit).
            // Never leave SeamHold. **no equivalent** (playlist scrub local).
            idle.set_scrub_cache_policy(ScrubCachePolicy::On);
            idle.prepare_seam_at(aim.frame.source()).await?;
        }
        if !pl.open_gen.is_current(epoch) {
            return Ok(());
        }
        if pl.live_clip.get_untracked() != aim.clip_index && !try_promote_idle(pl, aim.clip_index) {
            swap_live_idle(pl, aim.clip_index);
        }
        // Aim local before neighbor preload — mirror must not see new live +
        // stale idle local. **no equivalent**.
        pl.live_video().playhead.set(aim.frame.source());
        let neighbor = idle_neighbor_for_zone(
            &pl.model.get_untracked(),
            aim.clip_index,
            pl.looping.get_untracked(),
            pl.loop_zone().0,
            pl.loop_zone().1,
        );
        preload_idle(pl, neighbor, epoch).await;
        if !pl.open_gen.is_current(epoch) {
            return Ok(());
        }
        sync_live_loop_from_playlist(pl);
        let live = pl.live_video();
        live.ended.set(false);
        live.playhead.set(aim.frame.source());
        if then_play {
            // SeamHold already painted — PlayPrime, no second Exact flash.
            pl.play();
        } else {
            settle_playlist_pause_exact(pl, live, aim.frame.source(), slot_exact_hit);
        }
        return Ok(());
    }

    if !pl.open_gen.is_current(epoch) {
        return Ok(());
    }
    sync_live_loop_from_playlist(pl);
    let live = pl.live_video();
    live.ended.set(false);
    // Slot playhead is local — PlayPrime reads it (playlist global ≠ slot).
    live.playhead.set(aim.frame.source());
    if then_play {
        pl.play();
    } else {
        settle_playlist_pause_exact(pl, live, aim.frame.source(), false);
    }
    Ok(())
}

/// Pause Exact from slot blit: PresentHit → present; miss → seek densify.
///
/// `known_hit` skips a second blit check after cross-clip promote already checked.
/// Always re-assert live On — open/park leave HostCtx Off and Exact ingest is
/// gated by `scrub_cache_off`. **no equivalent** (playlist scrub local).
fn settle_playlist_pause_exact(
    _pl: VideoPlaylistRef,
    live: crate::VideoRef,
    local: u32,
    known_hit: bool,
) {
    use crate::virtual_timeline::{ExactSettlePlan, plan_exact_settle};
    live.set_scrub_cache_policy(ScrubCachePolicy::On);
    let hit = known_hit || live.has_scrub_exact_blit(local);
    if matches!(plan_exact_settle(hit), ExactSettlePlan::PresentHit)
        && live.try_present_scrub_exact(local)
    {
        // prepare_seam / cancelled seek can leave ExactHold — arm alone only
        // latches densify. Parity with host_ops PresentHit. **no equivalent**.
        live.clear_densify_hold();
        live.arm_scrub_densify_after_exact(local);
        return;
    }
    live.seek_frame(local);
}

/// Host promote lead for a local aim. One table for [`super::loop_seam_exec`]
/// promote + [`maybe_kick_idle_seam_warm`] — no `if local==0` fork at call sites.
/// Play path: idle **main** `SeamHold` (not PreferSoftware stash). **no equivalent**.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PromoteLead {
    /// Frame 0 — idle `arm_seam_hold(0)` / near-EOF kick.
    Origin,
    /// Mid-clip (LoopZone `loop_in`) — idle `arm_seam_hold(n)`.
    Mid { local: u32 },
}

impl PromoteLead {
    pub(crate) fn for_local(local: u32) -> Self {
        if local == 0 {
            Self::Origin
        } else {
            Self::Mid { local }
        }
    }

    pub(crate) fn local(self) -> u32 {
        match self {
            Self::Origin => 0,
            Self::Mid { local } => local,
        }
    }

    /// Idle Play-ready before swap (`SeamHold` on main HW). Paint-only
    /// `prepare_seam_at` = arm-fail fallback **and** paused `seek_to_pos`
    /// (never leave SeamHold without Exact densify). Soft-cancel → retry arm once.
    pub(crate) async fn prepare(self, video: VideoRef) -> Result<(), MediaError> {
        let local = self.local();
        seam_trace(&format!("prepare lead={self:?} local={local} (seam hold)"));
        let soft = |e: &MediaError| e.is_cancelled() || is_soft_seam_cancel(&e.to_string());
        match video.arm_seam_hold(local).await {
            Ok(()) => Ok(()),
            Err(e) if soft(&e) => {
                seam_trace("arm soft-cancel retry");
                match video.arm_seam_hold(local).await {
                    Ok(()) => Ok(()),
                    Err(e2) if soft(&e2) => {
                        seam_trace("arm soft-cancel → paint fallback");
                        video.prepare_seam_at(local).await
                    }
                    Err(e2) => {
                        seam_trace(&format!("arm fail → paint fallback: {e2}"));
                        video.prepare_seam_at(local).await
                    }
                }
            }
            Err(e) => {
                seam_trace(&format!("arm fail → paint fallback: {e}"));
                video.prepare_seam_at(local).await
            }
        }
    }

    /// Near-wall fire-and-forget: idle main Play-ready (`SeamHold`).
    pub(crate) fn kick_warm(self, video: VideoRef) {
        video.kick_arm_seam_hold(self.local());
    }
}

/// Stop the live slot without clearing playlist play intent (facade `playing`).
/// Host wall must not keep Play while idle `prepare` runs — overrun past
/// `zone_last` with mirror gated → video past loop, scrubber frozen (mobile).
/// **no equivalent** (playlist host).
pub(crate) fn stop_live_for_host_seam(pl: VideoPlaylistRef) {
    let live = pl.live_video();
    if live.playing.get_untracked() || live.ended.get_untracked() {
        seam_trace("stop live before prepare");
        live.pause_hard_no_densify();
    }
}

/// After a cancelled seam that already stopped live: resume if facade still wants Play.
pub(crate) fn resume_live_if_playlist_playing(pl: VideoPlaylistRef) {
    if !pl.playing.get_untracked() {
        return;
    }
    let live = pl.live_video();
    live.ended.set(false);
    live.play();
}

/// Next clip index for Play All seam — [`plan_seam_next_clip`].
pub(crate) fn seam_next_clip(pl: VideoPlaylistRef, cur: u32) -> Option<u32> {
    let model = pl.model.get_untracked();
    let looping = pl.looping.get_untracked();
    let (zone_in, zone_out) = pl.loop_zone();
    let total = model.total_frame_count();
    let zone_full = zone_in == 0 && zone_out == total;
    let next_clip = model.next_clip_index(cur);
    let next_start = next_clip
        .and_then(|n| model.clip_start_frame(n).map(GlobalFrame::get))
        .unwrap_or(0);
    let zone_in_clip = model
        .at_global_frame(GlobalFrame::new(zone_in))
        .map(|p| p.clip_index);
    plan_seam_next_clip(SeamNextCtx {
        cur,
        next_clip,
        next_start,
        zone_in_clip,
        looping,
        zone_full,
        zone_out,
        clip_count: model.len() as u32,
    })
}

/// Prewarm kick target from [`seam_warm_schedule`].
#[derive(Clone, Copy, Debug)]
struct SeamWarmTarget {
    clip_index: u32,
    lead: PromoteLead,
    /// `true` → idle slot must already hold `clip_index`; `false` → live (same-clip Host).
    on_idle: bool,
}

/// When to near-wall warm: `LoopOwner` + zone geometry + media time → lead.
/// `SeamPhase` busy / Engine loop warm are gated in [`maybe_kick_idle_seam_warm`].
/// Shape: Remotion `prewarmIteratorForLooping` — **no equivalent** (playlist local).
fn seam_warm_schedule(pl: VideoPlaylistRef) -> Option<SeamWarmTarget> {
    let live = pl.live_video();
    let model = pl.model.get_untracked();
    let cur = pl.live_clip.get_untracked();
    let clip = model.clip(cur as usize)?;
    let t = live.timestamp.get_untracked();
    let (zone_in, zone_out) = pl.loop_zone();
    let total = model.total_frame_count();
    let zone_full = zone_in == 0 && zone_out == total;
    let looping = pl.looping.get_untracked();
    let owner = loop_owner(pl);

    // 1. Host LoopZone wall — global time to `zone_out` (idle neighbor or same-clip live).
    if matches!(owner, LoopOwner::Host) && looping && !zone_full {
        let wall_t = model
            .at_global_frame(GlobalFrame::new(zone_out.saturating_sub(1)))
            .and_then(|p| {
                let start = model.clip_start_secs(p.clip_index as usize)?;
                let c = model.clip(p.clip_index as usize)?;
                Some(start + c.time_at_frame(p.frame.source()))
            });
        let cur_t = model
            .clip_start_secs(cur as usize)
            .map(|s| s + t)
            .unwrap_or(t);
        // Only when wrap aim is on idle — never Mid/Origin prepare on Playing
        // live (seek/`begin_op` freezes chrome; small Host zones re-enter the
        // 1s prewarm window immediately after wrap). Same-clip Host paints at
        // wall in `wrap_to_loop_in`. **no equivalent**.
        if wall_t.is_some_and(|w| should_prewarm_loop_origin(cur_t, w, LOOP_ORIGIN_PREWARM_SECS))
            && let Some(aim) = model.at_global_frame(GlobalFrame::new(zone_in))
            && aim.clip_index != cur
        {
            return Some(SeamWarmTarget {
                clip_index: aim.clip_index,
                lead: PromoteLead::for_local(aim.frame.source()),
                on_idle: true,
            });
        }
    }

    // 2. Near live clip EOF — cut promote / next-in-zone (idle only).
    if !should_prewarm_loop_origin(t, clip.duration_secs, LOOP_ORIGIN_PREWARM_SECS) {
        return None;
    }
    if let Some(next_i) = seam_next_clip(pl, cur) {
        let local = if looping
            && !zone_full
            && let Some(aim) = model.at_global_frame(GlobalFrame::new(zone_in))
            && aim.clip_index == next_i
        {
            aim.frame.source()
        } else {
            0
        };
        return Some(SeamWarmTarget {
            clip_index: next_i,
            lead: PromoteLead::for_local(local),
            on_idle: true,
        });
    }
    // Single-clip Host wrap: engine LoopSeam when zone ⊆ clip; else prepare at
    // wall — no live kick_prepare while Playing.
    None
}

/// Near-EOF / near LoopZone wall: apply [`seam_warm_schedule`] → [`PromoteLead::kick_warm`].
/// **no equivalent** (playlist local).
pub(crate) fn maybe_kick_idle_seam_warm(pl: VideoPlaylistRef) {
    if seam_busy(pl) {
        return;
    }
    let live = pl.live_video();
    if !live.playing.get_untracked() || !live.ready.get_untracked() {
        return;
    }
    // Engine owns intra-clip LoopSeam warm (`maybe_kick_loop_origin` on the slot).
    if matches!(loop_owner(pl), LoopOwner::Engine) {
        return;
    }
    let Some(target) = seam_warm_schedule(pl) else {
        return;
    };
    if pl.latches.get_value().seam_warm_for == Some(target.clip_index) {
        return;
    }
    // Idle only — live `kick_prepare` while Playing clears the play session
    // (Host small LoopZone: pause icon, frozen progress). **no equivalent**.
    if !target.on_idle {
        seam_trace(&format!(
            "warm skip live clip={} lead={:?}",
            target.clip_index, target.lead
        ));
        return;
    }
    if pl.idle_clip.get_untracked() != Some(target.clip_index) {
        return;
    }
    let idle = pl.idle_video();
    if !idle.ready.get_untracked() {
        return;
    }
    pl.latches
        .update_value(|l| l.seam_warm_for = Some(target.clip_index));
    target.lead.kick_warm(idle);
}
