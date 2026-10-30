//! Live/idle slot helpers for [`super::VideoPlaylist`].

use fpmedia_web::timer::sleep_ms;
use fpmedia_web::{ScrubCachePolicy, ThumbPolicy, VideoMetadata};
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

/// Prefer next clip as idle neighbor; else previous.
pub(crate) fn idle_neighbor(model: &PlaylistModel, live_clip: u32) -> Option<u32> {
    model
        .next_clip_index(live_clip)
        .or_else(|| live_clip.checked_sub(1))
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
    idle.set_thumb_policy(ThumbPolicy::Off);
    idle.set_scrub_cache_policy(ScrubCachePolicy::Off);
    idle.close();
    sleep_ms(0).await;
    if pl.open_gen.get_value() != epoch {
        return Err("cancelled".into());
    }
    src.open_on(idle);
    pl.idle_clip.set(Some(clip_index));
    wait_ready(idle, epoch, pl.open_gen).await?;
    Ok(())
}

pub(crate) fn swap_live_idle(pl: VideoPlaylistRef, new_live_clip: u32, new_idle_clip: Option<u32>) {
    pl.live_is_a.set(!pl.live_is_a.get_untracked());
    pl.live_clip.set(new_live_clip);
    pl.idle_clip.set(new_idle_clip);
    let live = pl.live_video();
    live.set_loop(false);
    live.set_thumb_policy(ThumbPolicy::On);
    live.set_scrub_cache_policy(ScrubCachePolicy::On);
    live.ended.set(false);
    let idle = pl.idle_video();
    idle.set_thumb_policy(ThumbPolicy::Off);
    idle.set_scrub_cache_policy(ScrubCachePolicy::Off);
    idle.set_loop(false);
}

/// Sync promote when idle already holds `clip_index` and is ready.
pub(crate) fn try_promote_idle(pl: VideoPlaylistRef, clip_index: u32) -> bool {
    if pl.live_clip.get_untracked() == clip_index {
        return true;
    }
    if pl.idle_clip.get_untracked() != Some(clip_index) || !pl.idle_video().ready.get_untracked() {
        return false;
    }
    let model = pl.model.get_untracked();
    let neighbor = idle_neighbor(&model, clip_index);
    swap_live_idle(pl, clip_index, neighbor);
    true
}

pub(crate) async fn preload_idle(pl: VideoPlaylistRef, clip_index: Option<u32>, epoch: u64) {
    let Some(i) = clip_index else {
        return;
    };
    if pl.open_gen.get_value() != epoch {
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
    idle.set_thumb_policy(ThumbPolicy::Off);
    idle.set_scrub_cache_policy(ScrubCachePolicy::Off);
    src.open_on(idle);
    pl.idle_clip.set(Some(i));
    let _ = wait_ready(idle, epoch, pl.open_gen).await;
}

/// Make `aim.clip_index` live (swap or open), then seek to `aim.local_frame`.
pub(crate) async fn seek_to_pos(
    pl: VideoPlaylistRef,
    aim: crate::video_playlist::PlaylistPos,
    epoch: u64,
) -> Result<(), String> {
    if pl.open_gen.get_value() != epoch {
        return Ok(());
    }
    pl.ended.set(false);
    pl.pos.set(aim);
    if let Some(ms) = pl.model.get_untracked().ms_at(aim) {
        pl.playhead_ms.set(ms);
    }

    if pl.live_clip.get_untracked() != aim.clip_index {
        if !try_promote_idle(pl, aim.clip_index) {
            ensure_clip_on_idle(pl, aim.clip_index, epoch).await?;
            if pl.open_gen.get_value() != epoch {
                return Ok(());
            }
            let neighbor = idle_neighbor(&pl.model.get_untracked(), aim.clip_index);
            swap_live_idle(pl, aim.clip_index, neighbor);
        }
        let neighbor = pl.idle_clip.get_untracked();
        preload_idle(pl, neighbor, epoch).await;
    }

    if pl.open_gen.get_value() != epoch {
        return Ok(());
    }
    let live = pl.live_video();
    live.set_loop(false);
    live.ended.set(false);
    live.seek_frame(aim.local_frame);
    Ok(())
}

/// Start live at local 0 after a promote. Idle open already Exact-settled to 0 —
/// skip another Exact seek when still aimed there (PlayPrime paints via iterator).
/// **no equivalent** (playlist seam local); Play = mediabunny media-player seek→iter→play.
fn play_seam_from_origin(live: &VideoRef) {
    live.ended.set(false);
    if live.playhead.get_untracked() != 0 {
        live.seek_frame(0);
    }
    live.play();
}

/// After live clip EOF: play next, wrap on playlist loop, or mark playlist ended.
pub(crate) async fn advance_after_ended(pl: VideoPlaylistRef, epoch: u64) -> Result<(), String> {
    if pl.open_gen.get_value() != epoch {
        return Ok(());
    }
    let model = pl.model.get_untracked();
    let cur = pl.live_clip.get_untracked();

    if let Some(next_i) = model.next_clip_index(cur) {
        pl.status.set(format!("Seam → clip {}…", next_i + 1));
        ensure_clip_on_idle(pl, next_i, epoch).await?;
        if pl.open_gen.get_value() != epoch {
            return Ok(());
        }
        let following = model.next_clip_index(next_i);
        swap_live_idle(pl, next_i, following);
        play_seam_from_origin(&pl.live_video());
        pl.ended.set(false);
        pl.status
            .set(format!("Playing clip {}/{}", next_i + 1, model.len()));
        preload_idle(pl, following, epoch).await;
        return Ok(());
    }

    if pl.looping.get_untracked() {
        pl.status.set("Loop → clip 1…".into());
        if model.len() <= 1 {
            // Same clip at EOF — must Exact-seek origin (Play-on-last-frame).
            let live = pl.live_video();
            live.ended.set(false);
            live.seek_frame(0);
            live.play();
            pl.ended.set(false);
            return Ok(());
        }
        ensure_clip_on_idle(pl, 0, epoch).await?;
        if pl.open_gen.get_value() != epoch {
            return Ok(());
        }
        let following = model.next_clip_index(0);
        swap_live_idle(pl, 0, following);
        play_seam_from_origin(&pl.live_video());
        pl.ended.set(false);
        pl.status.set(format!("Playing clip 1/{}", model.len()));
        preload_idle(pl, following, epoch).await;
        return Ok(());
    }

    pl.ended.set(true);
    pl.status.set("Ended.".into());
    Ok(())
}
