//! Live/idle slot helpers for [`super::VideoPlaylist`].

use fpmedia_web::timer::sleep_ms;
use fpmedia_web::{
    LOOP_ORIGIN_PREWARM_SECS, ScrubCachePolicy, VideoMetadata, should_prewarm_loop_origin,
};
use leptos::prelude::*;

use crate::VideoRef;
use crate::video_playlist::PlaylistModel;

use super::video_playlist_ref::VideoPlaylistRef;

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

/// Flip live/idle slots. `idle_clip` becomes the clip that **physically** moved
/// to idle (former live) — never an aspirational neighbor (that lied and made
/// scrub promote the wrong media). Caller [`preload_idle`]s the desired neighbor.
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
    live.set_loop(false);
    live.set_scrub_cache_policy(ScrubCachePolicy::On);
    live.ended.set(false);
    let idle = pl.idle_video();
    idle.set_scrub_cache_policy(ScrubCachePolicy::Off);
    idle.set_loop(false);
    idle.ended.set(false);
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

/// Make `aim.clip_index` live (swap or open), then seek to `aim.local_frame`.
/// `then_play`: after seek, start Play (cross-clip scrub resume — playlist local).
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
    let live = pl.live_video();
    live.set_loop(false);
    live.ended.set(false);
    // Slot playhead is local — PlayPrime reads it (playlist global ≠ slot).
    // Without this, promote@0 + play() starts at clip origin then seeks (flash /
    // densify), or worse races an EOF seam to the next clip.
    live.playhead.set(aim.local_frame);
    live.seek_frame(aim.local_frame);
    if then_play {
        live.play();
    }
    Ok(())
}

/// Start live at local 0 after a promote.
///
/// PreferSoftware origin stash (near-EOF kick) → PlayPrime sync paint; miss →
/// cold `start_play_iter`. Avoid `seek_frame(0)` when already at origin — seek
/// drops the stash. **no equivalent** (playlist local); engine cousin =
/// `start_loop_seam_with_stash` / Remotion `prewarmIteratorForLooping`.
async fn play_seam_from_origin(live: &VideoRef) {
    live.ended.set(false);
    // UI playhead only — do not seek (would `drop_origin_stash`). Idle opens @0.
    if live.playhead.get_untracked() != 0 {
        live.playhead.set(0);
    }
    live.warm_play_origin().await;
    live.play();
}

/// Next clip index for Play All seam. LoopZone: only advance while next starts
/// before `zone_out`; else wrap clip owning `zone_in` (or `None` → same-clip host
/// wrap). Full axis + looping → 0 when past last.
fn seam_next_clip(pl: VideoPlaylistRef, cur: u32) -> Option<u32> {
    let model = pl.model.get_untracked();
    let looping = pl.looping.get_untracked();
    let (zone_in, zone_out) = pl.loop_zone();
    let total = model.total_frame_count();
    let zone_full = zone_in == 0 && zone_out == total;

    if let Some(next) = model.next_clip_index(cur) {
        let next_start = model.clip_start_frame(next).unwrap_or(0);
        if !looping || zone_full || next_start < zone_out {
            return Some(next);
        }
        // Next would leave the zone — wrap to zone-in clip (if different).
        return model.at_global_frame(zone_in).and_then(|p| {
            if p.clip_index != cur {
                Some(p.clip_index)
            } else {
                None
            }
        });
    }
    if looping && model.len() > 1 {
        return model
            .at_global_frame(zone_in)
            .map(|p| p.clip_index)
            .or(Some(0));
    }
    None
}

/// Near-EOF / near LoopZone wall: warm idle wrap target (origin or `loop_in`).
/// Shape: Remotion `prewarmIteratorForLooping` — **no equivalent** (playlist local).
pub(crate) fn maybe_kick_idle_seam_warm(pl: VideoPlaylistRef) {
    let live = pl.live_video();
    if !live.playing.get_untracked() || !live.ready.get_untracked() {
        return;
    }
    let model = pl.model.get_untracked();
    let cur = pl.live_clip.get_untracked();
    let Some(clip) = model.clip(cur as usize) else {
        return;
    };
    let t = live.timestamp.get_untracked();
    let (zone_in, zone_out) = pl.loop_zone();
    let total = model.total_frame_count();
    let zone_full = zone_in == 0 && zone_out == total;
    let looping = pl.looping.get_untracked();

    // Mid LoopZone wall: warm idle at `zone_in` (may be mid-clip) before host wrap.
    if looping && !zone_full {
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
        if wall_t.is_some_and(|w| should_prewarm_loop_origin(cur_t, w, LOOP_ORIGIN_PREWARM_SECS))
            && let Some(aim) = model.at_global_frame(zone_in)
        {
            if pl.seam_warm_for.get_value() == Some(aim.clip_index) {
                return;
            }
            if pl.idle_clip.get_untracked() != Some(aim.clip_index) {
                return;
            }
            let idle = pl.idle_video();
            if !idle.ready.get_untracked() {
                return;
            }
            pl.seam_warm_for.set_value(Some(aim.clip_index));
            idle.kick_prepare_seam_at(aim.local_frame);
            return;
        }
    }

    if !should_prewarm_loop_origin(t, clip.duration_secs, LOOP_ORIGIN_PREWARM_SECS) {
        return;
    }

    if let Some(next_i) = seam_next_clip(pl, cur) {
        if pl.seam_warm_for.get_value() == Some(next_i) {
            return;
        }
        if pl.idle_clip.get_untracked() != Some(next_i) {
            return;
        }
        let idle = pl.idle_video();
        if !idle.ready.get_untracked() {
            return;
        }
        // Zone wrap target mid-clip → warm `loop_in`, not origin.
        if looping
            && !zone_full
            && let Some(aim) = model.at_global_frame(zone_in)
            && aim.clip_index == next_i
        {
            pl.seam_warm_for.set_value(Some(next_i));
            idle.kick_prepare_seam_at(aim.local_frame);
            return;
        }
        pl.seam_warm_for.set_value(Some(next_i));
        idle.kick_warm_play_origin();
        return;
    }

    if looping && model.len() == 1 {
        if pl.seam_warm_for.get_value() == Some(cur) {
            return;
        }
        pl.seam_warm_for.set_value(Some(cur));
        if !zone_full {
            live.kick_prepare_seam_at(zone_in.min(clip.last_frame()));
        } else {
            live.kick_warm_play_origin();
        }
    }
}

/// Host LoopZone wrap → paint `zone_in` on idle → swap → Play.
/// Same shape as clip seam (`prepare_seam` before promote). Slots stay
/// non-looping. **no equivalent** (playlist local).
pub(crate) async fn wrap_to_loop_in(pl: VideoPlaylistRef, epoch: u64) -> Result<(), String> {
    if pl.open_gen.get_value() != epoch {
        pl.zone_wrapping.set_value(false);
        return Ok(());
    }
    pl.zone_wrapping.set_value(true);
    let result = async {
        let (zone_in, zone_out) = pl.loop_zone();
        let model = pl.model.get_untracked();
        let Some(aim) = model.at_global_frame(zone_in) else {
            return Ok(());
        };
        pl.status.set(format!("LoopZone → frame {zone_in}…"));
        pl.seam_warm_for.set_value(None);
        pl.ended.set(false);
        if let Some(g) = model.global_frame_at(aim) {
            pl.playhead.set(g);
        }
        pl.pos.set(aim);

        // Same-clip wrap: paint on live, then play (no slot flip).
        if pl.live_clip.get_untracked() == aim.clip_index {
            let live = pl.live_video();
            live.set_loop(false);
            live.ended.set(false);
            live.prepare_seam_at(aim.local_frame).await?;
            if pl.open_gen.get_value() != epoch {
                return Ok(());
            }
            live.playhead.set(aim.local_frame);
            live.play();
            return Ok(());
        }

        ensure_clip_on_idle(pl, aim.clip_index, epoch).await?;
        if pl.open_gen.get_value() != epoch {
            return Ok(());
        }
        let idle = pl.idle_video();
        idle.prepare_seam_at(aim.local_frame).await?;
        if pl.open_gen.get_value() != epoch {
            return Ok(());
        }
        // Pause outgoing only at swap — no cold pause@out waiting for seek.
        swap_live_idle(pl, aim.clip_index);
        let live = pl.live_video();
        live.ended.set(false);
        live.playhead.set(aim.local_frame);
        live.play();
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
    pl.zone_wrapping.set_value(false);
    result
}

/// After live clip EOF: ensure next on idle → paint origin → swap → `play_seam`.
/// `from_clip` = ended clip (ignore if live already moved).
pub(crate) async fn advance_after_ended(
    pl: VideoPlaylistRef,
    epoch: u64,
    from_clip: u32,
) -> Result<(), String> {
    if pl.open_gen.get_value() != epoch {
        return Ok(());
    }
    if pl.zone_wrapping.get_value() {
        return Ok(());
    }
    if pl.live_clip.get_untracked() != from_clip {
        return Ok(());
    }
    if pl.live_video().playing.get_untracked() && !pl.live_video().ended.get_untracked() {
        return Ok(());
    }
    let model = pl.model.get_untracked();
    let cur = from_clip;
    let (zone_in, zone_out) = pl.loop_zone();
    let total = model.total_frame_count();
    let zone_full = zone_in == 0 && zone_out == total;

    // LoopZone ends inside this clip (or at its end): EOF must wrap to `in`,
    // never seam to the next clip origin (was: flash 2nd-clip@0 then jump to in).
    if pl.looping.get_untracked() && !zone_full {
        let clip_end = model.clip_end_frame(cur).unwrap_or(0);
        if zone_out <= clip_end {
            return wrap_to_loop_in(pl, epoch).await;
        }
    }

    if let Some(next_i) = seam_next_clip(pl, cur) {
        pl.status.set(format!("Seam → clip {}…", next_i + 1));
        ensure_clip_on_idle(pl, next_i, epoch).await?;
        if pl.open_gen.get_value() != epoch || pl.live_clip.get_untracked() != from_clip {
            return Ok(());
        }
        // Zone wrap to mid-clip in → seek+play (not frame-0 origin seam).
        if pl.looping.get_untracked()
            && !zone_full
            && let Some(aim) = model.at_global_frame(zone_in)
            && aim.clip_index == next_i
            && aim.local_frame > 0
        {
            return wrap_to_loop_in(pl, epoch).await;
        }
        let following = idle_neighbor_for_zone(
            &model,
            next_i,
            pl.looping.get_untracked(),
            zone_in,
            zone_out,
        );
        let idle = pl.idle_video();
        // Origin on idle stage **before** CSS promote — else flash former-live
        // last frame (ensure early-return left EOF on wrap / reused slot).
        idle.prepare_seam_origin().await?;
        if pl.open_gen.get_value() != epoch || pl.live_clip.get_untracked() != from_clip {
            return Ok(());
        }
        swap_live_idle(pl, next_i);
        pl.seam_warm_for.set_value(None);
        play_seam_from_origin(&pl.live_video()).await;
        pl.ended.set(false);
        pl.status
            .set(format!("Playing clip {}/{}", next_i + 1, model.len()));
        preload_idle(pl, following, epoch).await;
        return Ok(());
    }

    if pl.looping.get_untracked() {
        // Same-clip / zone wrap (no sequential next inside zone).
        return wrap_to_loop_in(pl, epoch).await;
    }

    pl.ended.set(true);
    pl.status.set("Ended.".into());
    Ok(())
}
