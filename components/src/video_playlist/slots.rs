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
        let neighbor = idle_neighbor(
            &pl.model.get_untracked(),
            aim.clip_index,
            pl.looping.get_untracked(),
        );
        preload_idle(pl, neighbor, epoch).await;
    }

    if pl.open_gen.get_value() != epoch {
        return Ok(());
    }
    let live = pl.live_video();
    live.set_loop(false);
    live.ended.set(false);
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

/// Next clip index for Play All seam (playlist loop → 0 when past last).
fn seam_next_clip(pl: VideoPlaylistRef, cur: u32) -> Option<u32> {
    let model = pl.model.get_untracked();
    model.next_clip_index(cur).or_else(|| {
        if pl.looping.get_untracked() && model.len() > 1 {
            Some(0)
        } else {
            None
        }
    })
}

/// Near-EOF: kick frame-0 warm on idle next (or live when single-clip loop).
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
        pl.seam_warm_for.set_value(Some(next_i));
        idle.kick_warm_play_origin();
        return;
    }

    if pl.looping.get_untracked() && model.len() == 1 {
        if pl.seam_warm_for.get_value() == Some(cur) {
            return;
        }
        pl.seam_warm_for.set_value(Some(cur));
        live.kick_warm_play_origin();
    }
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
    if pl.live_clip.get_untracked() != from_clip {
        return Ok(());
    }
    if pl.live_video().playing.get_untracked() && !pl.live_video().ended.get_untracked() {
        return Ok(());
    }
    let model = pl.model.get_untracked();
    let cur = from_clip;

    if let Some(next_i) = seam_next_clip(pl, cur) {
        pl.status.set(format!("Seam → clip {}…", next_i + 1));
        ensure_clip_on_idle(pl, next_i, epoch).await?;
        if pl.open_gen.get_value() != epoch || pl.live_clip.get_untracked() != from_clip {
            return Ok(());
        }
        let following = idle_neighbor(&model, next_i, pl.looping.get_untracked());
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
        pl.status.set("Loop → clip 1…".into());
        if model.len() <= 1 {
            let live = pl.live_video();
            live.ended.set(false);
            live.seek_frame(0);
            live.warm_play_origin().await;
            live.play();
            pl.seam_warm_for.set_value(None);
            pl.ended.set(false);
            return Ok(());
        }
        // Unreachable: seam_next_clip covers multi loop → 0.
        return Ok(());
    }

    pl.ended.set(true);
    pl.status.set("Ended.".into());
    Ok(())
}
