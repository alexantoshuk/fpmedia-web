//! Live/idle slot helpers for [`super::VideoPlaylist`].

use fpmedia_web::timer::sleep_ms;
use fpmedia_web::{
    LOOP_ORIGIN_PREWARM_SECS, ScrubCachePolicy, VideoMetadata, should_prewarm_loop_origin,
};
use leptos::prelude::*;

use crate::VideoRef;
use crate::video_playlist::PlaylistModel;

use super::host_zone::{HostZoneOwner, is_soft_seam_cancel};
use super::loop_seam::{
    LoopOwner, SeamPhase, end_seam, ensure_seam_preparing, loop_owner, seam_busy, seam_trace,
    set_seam_phase, try_begin_seam,
};
use super::plan::{
    AdvanceEndedCtx, AdvanceEndedPlan, SeamNextCtx, WrapDonePlan, plan_advance_ended,
    plan_seam_next_clip, plan_wrap_done,
};
use super::video_playlist_ref::VideoPlaylistRef;

/// Snap playlist global aim to `zone_in` so wall cannot re-fire on stale `g`.
fn reassert_zone_in_playhead(pl: VideoPlaylistRef) {
    let (zone_in, _) = pl.loop_zone();
    let model = pl.model.get_untracked();
    let Some(aim) = model.at_global_frame(zone_in) else {
        return;
    };
    pl.pos.set(aim);
    if let Some(g) = model.global_frame_at(aim) {
        pl.playhead.set(g);
    }
}

pub(crate) const PROBE_TIMEOUT_MS: f64 = 120_000.0;
pub(crate) const PROBE_POLL_MS: i32 = 32;

pub(crate) async fn wait_ready(
    video: VideoRef,
    epoch: u64,
    open_gen: StoredValue<u64, LocalStorage>,
) -> Result<VideoMetadata, String> {
    let start = fpmedia_web::timer::now_ms().unwrap_or(0.0);
    loop {
        if open_gen.get_value() != epoch {
            return Err("cancelled".into());
        }
        if let Some(err) = video.error.get_untracked() {
            return Err(err);
        }
        if video.ready.get_untracked()
            && let Some(meta) = video.metadata.get_untracked()
        {
            return Ok(meta);
        }
        let now = fpmedia_web::timer::now_ms().unwrap_or(start);
        if now - start > PROBE_TIMEOUT_MS {
            return Err("open timed out".into());
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
        let next_start = model.clip_start_frame(next).unwrap_or(0);
        if next_start < zone_out {
            return Some(next);
        }
    }
    model
        .at_global_frame(zone_in)
        .map(|p| p.clip_index)
        .or_else(|| idle_neighbor(model, live_clip, true))
}

pub(crate) async fn ensure_clip_on_idle(
    pl: VideoPlaylistRef,
    clip_index: u32,
    epoch: u64,
) -> Result<(), String> {
    if pl.open_gen.get_value() != epoch {
        return Err("cancelled".into());
    }
    if pl.idle_clip.get_untracked() == Some(clip_index) && pl.idle_video().ready.get_untracked() {
        return Ok(());
    }
    let src = pl
        .sources
        .try_with_value(|s| s.get(clip_index as usize).cloned())
        .flatten()
        .ok_or_else(|| format!("missing source for clip {}", clip_index + 1))?;
    let idle = pl.idle_video();
    idle.set_loop(false);
    idle.set_scrub_cache_policy(ScrubCachePolicy::Off);
    idle.close();
    sleep_ms(0).await;
    if pl.open_gen.get_value() != epoch {
        return Err("cancelled".into());
    }
    src.open_on(idle);
    pl.idle_clip.set(Some(clip_index));
    wait_ready(idle, epoch, pl.open_gen).await?;
    // Open resets engine gain — re-apply playlist-wide volume/mute.
    pl.apply_gain_to_slots();
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
            let Some(clip_start) = model.clip_start_frame(live_i) else {
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
/// Policy On = scrub-cache **capability** only (does not arm densify). Pause fill
/// comes from seek / EnterScrub / pause doors; seam Play never arms. **no equivalent**.
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
    idle.set_scrub_cache_policy(ScrubCachePolicy::Off);
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

pub(crate) async fn preload_idle(pl: VideoPlaylistRef, clip_index: Option<u32>, epoch: u64) {
    let Some(i) = clip_index else {
        return;
    };
    if pl.open_gen.get_value() != epoch {
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
    let idle = pl.idle_video();
    idle.set_loop(false);
    idle.set_scrub_cache_policy(ScrubCachePolicy::Off);
    src.open_on(idle);
    pl.idle_clip.set(Some(i));
    let _ = wait_ready(idle, epoch, pl.open_gen).await;
    pl.apply_gain_to_slots();
}

/// Make `aim.clip_index` live (swap or open), then internal-seek to `aim.local_frame`.
/// `then_play`: after seek, playlist `play()` (Host outside LoopZone → wrap).
pub(crate) async fn seek_to_pos(
    pl: VideoPlaylistRef,
    aim: crate::video_playlist::PlaylistPos,
    epoch: u64,
    then_play: bool,
) -> Result<(), String> {
    if pl.open_gen.get_value() != epoch {
        return Ok(());
    }
    pl.ended.set(false);
    pl.pos.set(aim);
    if let Some(g) = pl.model.get_untracked().global_frame_at(aim) {
        pl.playhead.set(g);
    }

    if pl.live_clip.get_untracked() != aim.clip_index {
        if !try_promote_idle(pl, aim.clip_index) {
            ensure_clip_on_idle(pl, aim.clip_index, epoch).await?;
            if pl.open_gen.get_value() != epoch {
                return Ok(());
            }
            swap_live_idle(pl, aim.clip_index);
        }
        let neighbor = idle_neighbor_for_zone(
            &pl.model.get_untracked(),
            aim.clip_index,
            pl.looping.get_untracked(),
            pl.loop_zone().0,
            pl.loop_zone().1,
        );
        preload_idle(pl, neighbor, epoch).await;
    }

    if pl.open_gen.get_value() != epoch {
        return Ok(());
    }
    sync_live_loop_from_playlist(pl);
    let live = pl.live_video();
    live.ended.set(false);
    // Slot playhead is local — PlayPrime reads it (playlist global ≠ slot).
    // Without this, promote@0 + play() starts at clip origin then seeks (flash /
    // densify), or worse races an EOF seam to the next clip.
    live.playhead.set(aim.local_frame);
    live.seek_frame(aim.local_frame);
    if then_play {
        // Playlist Play path (Host outside LoopZone → wrap). Not bare slot play.
        pl.play();
    }
    Ok(())
}

/// Host promote lead for a local aim. One table for [`promote_idle_at`] +
/// [`maybe_kick_idle_seam_warm`] — no `if local==0` fork at call sites.
/// Deeper PreferSoftware stash = parked (`state.md`). **no equivalent**.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PromoteLead {
    /// Frame 0 — PreferSoftware origin stash (`prepare_playlist_seam_at(0)` /
    /// near-EOF `kick_warm_play_origin`).
    Origin,
    /// Mid-clip (LoopZone `loop_in`) — densify-free `prepare_seam_at(n)`.
    Mid { local: u32 },
}

impl PromoteLead {
    fn for_local(local: u32) -> Self {
        if local == 0 {
            Self::Origin
        } else {
            Self::Mid { local }
        }
    }

    fn local(self) -> u32 {
        match self {
            Self::Origin => 0,
            Self::Mid { local } => local,
        }
    }

    /// Idle (or same-clip) paint before swap / wrap play.
    /// Engine `prepare_playlist_seam_at` `begin_op`s first — cancels in-flight
    /// near-wall `kick_prepare` before warm/seek (small-zone late-seek stop).
    async fn prepare(self, video: VideoRef) -> Result<(), String> {
        seam_trace(&format!("prepare lead={self:?} local={}", self.local()));
        match video.prepare_seam_at(self.local()).await {
            Ok(()) => Ok(()),
            // Near-wall kick / scrub settle can supersede once — retry once.
            Err(e) if is_soft_seam_cancel(&e) => {
                seam_trace("prepare soft-cancel retry");
                video.prepare_seam_at(self.local()).await
            }
            Err(e) => Err(e),
        }
    }

    /// Near-wall fire-and-forget. Origin uses the PlayPrime-join warm path;
    /// mid uses full prepare (lead + Exact/seek paint).
    fn kick_warm(self, video: VideoRef) {
        match self {
            Self::Origin => video.kick_warm_play_origin(),
            Self::Mid { local } => video.kick_prepare_seam_at(local),
        }
    }
}

/// Stop the live slot without clearing playlist play intent (facade `playing`).
/// Host wall must not keep Play while idle `prepare` runs — overrun past
/// `zone_last` with mirror gated → video past loop, scrubber frozen (mobile).
/// **no equivalent** (playlist host).
fn stop_live_for_host_seam(pl: VideoPlaylistRef) {
    let live = pl.live_video();
    if live.playing.get_untracked() || live.ended.get_untracked() {
        seam_trace("stop live before prepare");
        live.pause_hard_no_densify();
    }
}

/// After a cancelled seam that already stopped live: resume if facade still wants Play.
fn resume_live_if_playlist_playing(pl: VideoPlaylistRef) {
    if !pl.playing.get_untracked() {
        return;
    }
    let live = pl.live_video();
    live.ended.set(false);
    live.play();
}

/// Paint `local` on idle → swap → Play. Shared by clip cut (`Origin`) and
/// cross-clip LoopZone wrap (`Mid`). Prepare already warms/paints — no post-swap
/// `warm_play_origin` (was a cut@0 double warm). `expect_live`: abort if live
/// moved (EOF seam race). **no equivalent** (playlist host).
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
    let live = pl.live_video();
    live.ended.set(false);
    if live.playhead.get_untracked() != local {
        live.playhead.set(local);
    }
    set_seam_phase(pl, SeamPhase::Playing);
    pl.playing.set(true);
    live.play();
    Ok(())
}

/// Next clip index for Play All seam — [`plan_seam_next_clip`].
fn seam_next_clip(pl: VideoPlaylistRef, cur: u32) -> Option<u32> {
    let model = pl.model.get_untracked();
    let looping = pl.looping.get_untracked();
    let (zone_in, zone_out) = pl.loop_zone();
    let total = model.total_frame_count();
    let zone_full = zone_in == 0 && zone_out == total;
    let next_clip = model.next_clip_index(cur);
    let next_start = next_clip
        .and_then(|n| model.clip_start_frame(n))
        .unwrap_or(0);
    let zone_in_clip = model.at_global_frame(zone_in).map(|p| p.clip_index);
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
            .at_global_frame(zone_out.saturating_sub(1))
            .and_then(|p| {
                let start = model.clip_start_secs(p.clip_index as usize)?;
                let c = model.clip(p.clip_index as usize)?;
                Some(start + c.time_at_frame(p.local_frame))
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
            && let Some(aim) = model.at_global_frame(zone_in)
            && aim.clip_index != cur
        {
            return Some(SeamWarmTarget {
                clip_index: aim.clip_index,
                lead: PromoteLead::for_local(aim.local_frame),
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
            && let Some(aim) = model.at_global_frame(zone_in)
            && aim.clip_index == next_i
        {
            aim.local_frame
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
    if pl.seam_warm_for.get_value() == Some(target.clip_index) {
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
    pl.seam_warm_for.set_value(Some(target.clip_index));
    target.lead.kick_warm(idle);
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
    let result: Result<(), String> = async {
        let (zone_in, zone_out) = pl.loop_zone();
        let model = pl.model.get_untracked();
        let Some(aim) = model.at_global_frame(zone_in) else {
            resume_live_if_playlist_playing(pl);
            return Err("LoopZone in out of range".into());
        };
        pl.status.set(format!("LoopZone → frame {zone_in}…"));
        pl.seam_warm_for.set_value(None);
        pl.ended.set(false);
        if let Some(g) = model.global_frame_at(aim) {
            pl.playhead.set(g);
        }
        pl.pos.set(aim);

        // Same-clip wrap (fallback): paint + play. Prefer engine LoopSeam via
        // `sync_live_loop_from_playlist` — never leave `set_loop(false)` here
        // (that re-broke intra-clip sharing from `aa8ac99` / `2c19b70`).
        if pl.live_clip.get_untracked() == aim.clip_index {
            seam_trace(&format!("wrap same-clip prepare local={}", aim.local_frame));
            let lead = PromoteLead::for_local(aim.local_frame);
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
            live.play();
            return Ok(());
        }

        seam_trace(&format!(
            "wrap promote clip={} local={}",
            aim.clip_index, aim.local_frame
        ));
        promote_idle_at(pl, epoch, aim.clip_index, aim.local_frame, None).await?;
        if pl.open_gen.get_value() != epoch {
            return Ok(());
        }
        let neighbor = idle_neighbor_for_zone(
            &model,
            aim.clip_index,
            pl.looping.get_untracked(),
            zone_in,
            zone_out,
        );
        preload_idle(pl, neighbor, epoch).await;
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
    result
}

/// Begin Host LoopZone wrap (PlayPressed / wall Effect / scrub Up after preempt).
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
            .at_global_frame(zone_in)
            .map(|p| (p.clip_index, p.local_frame));
        let plan = plan_advance_ended(AdvanceEndedCtx {
            live_still_from_clip: pl.live_clip.get_untracked() == from_clip,
            owner: HostZoneOwner::from(loop_owner(pl)),
            zone_full,
            zone_out,
            clip_end: model.clip_end_frame(from_clip).unwrap_or(0),
            zone_in_aim,
            seam_next: seam_next_clip(pl, from_clip),
            looping: pl.looping.get_untracked(),
        });
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
