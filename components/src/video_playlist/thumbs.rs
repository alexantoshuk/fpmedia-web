//! Playlist Pause progressive thumbs on the same VT axis as scrub.
//!
//! Slot ThumbSink bitmaps stay ≤2. Progressive fills **only currently open**
//! live/idle — never `ensure_clip_on_idle` / clip walk (that stampeded opens →
//! `open timed out` after hover-around). Off-slot hover still warms idle via
//! [`VideoPlaylistRef`] paint path. Plan **ready** = open-slot bitmap (or
//! admit-skip). Local `schedule_thumb_key_fill` is off (`vt_thumb_authority`).
//! **no equivalent** (filmstrip / scrub local).

use std::collections::{HashMap, HashSet};

use leptos::prelude::*;
use wasm_bindgen_futures::spawn_local;

use crate::virtual_timeline::{
    ThumbFillPlanInput, merge_global_sync_keys, plan_next_thumb_fill, resolve_thumb_fill_playlist,
};

use super::video_playlist_ref::VideoPlaylistRef;

/// Refresh per-clip sync key cache from open live/idle slots.
pub(crate) fn refresh_thumb_sync_keys(pl: VideoPlaylistRef) {
    let mut cache = pl.thumb_sync_keys.get_value();
    let live_clip = pl.live_clip.get_untracked();
    let live = pl.live_video();
    if live.ready.get_untracked() {
        let keys = live.sync_frame_ids();
        if !keys.is_empty() {
            cache.insert(live_clip, keys.into_iter().map(u64::from).collect());
        }
    }
    if let Some(idle_idx) = pl.idle_clip.get_untracked() {
        let idle = pl.idle_video();
        if idle.ready.get_untracked() {
            let keys = idle.sync_frame_ids();
            if !keys.is_empty() {
                cache.insert(idle_idx, keys.into_iter().map(u64::from).collect());
            }
        }
    }
    pl.thumb_sync_keys.set_value(cache);
}

/// Kick one VT progressive step on open slots only. No-op while Play/scrub/ensure.
pub(crate) fn kick_vt_thumb_fill(pl: VideoPlaylistRef) {
    if !pl.ready.get_untracked() {
        return;
    }
    if pl.playing.get_untracked() || pl.scrubbing.get_untracked() {
        return;
    }
    if pl.thumb_fill_inflight.get_value() {
        return;
    }
    // Do not fight scrub/hover idle opens — that caused `open timed out`.
    if pl.ensure_idle_for.get_value().is_some() {
        return;
    }
    if seam_busy_for_thumbs(pl) {
        return;
    }

    refresh_thumb_sync_keys(pl);

    let model = pl.model.get_untracked();
    if model.is_empty() {
        return;
    }

    let cache = pl.thumb_sync_keys.get_value();
    let mut open_per_clip: Vec<(u32, Vec<u64>)> = Vec::with_capacity(2);
    let live_clip = pl.live_clip.get_untracked();
    if let Some(keys) = cache.get(&live_clip) {
        open_per_clip.push((live_clip, keys.clone()));
    }
    if let Some(idle_idx) = pl.idle_clip.get_untracked()
        && let Some(keys) = cache.get(&idle_idx)
    {
        open_per_clip.push((idle_idx, keys.clone()));
    }
    if open_per_clip.is_empty() {
        return;
    }
    let global_keys = merge_global_sync_keys(&model, &open_per_clip);

    let max_thumbs = pl
        .live_video()
        .thumb_max_thumbs()
        .max(pl.idle_video().thumb_max_thumbs())
        .max(1);

    let skip = pl.thumb_fill_skip.get_value();
    let ready = |g: u64| skip.contains(&g) || slot_thumb_ready(pl, g);

    let input = ThumbFillPlanInput {
        global_sync_keys: &global_keys,
        duration_secs: model.total_duration_secs(),
        max_thumbs,
    };

    let Some(global) = plan_next_thumb_fill(&input, ready) else {
        return;
    };
    let Some(aim) = resolve_thumb_fill_playlist(&model, global) else {
        return;
    };
    // Only fill if the clip is already on a slot — never open for progressive.
    let Some(video) = video_for_clip(pl, aim.clip_index) else {
        return;
    };
    if !video.ready.get_untracked() {
        return;
    }

    pl.thumb_fill_inflight.set_value(true);
    let epoch = pl.open_gen.get_value();
    let local = aim.local;
    spawn_local(async move {
        video.fill_thumb_key_at(local).await;
        if pl.open_gen.get_value() != epoch {
            pl.thumb_fill_inflight.set_value(false);
            return;
        }
        if video.thumb_has_key(local) {
            if let Some(h) = pl.exact_ledger() {
                h.borrow_mut().note_thumb(global);
            }
        } else {
            let mut skip = pl.thumb_fill_skip.get_value();
            skip.insert(global);
            pl.thumb_fill_skip.set_value(skip);
        }
        pl.thumb_fill_inflight.set_value(false);
        kick_vt_thumb_fill(pl);
    });
}

fn seam_busy_for_thumbs(pl: VideoPlaylistRef) -> bool {
    !matches!(
        pl.seam_phase.get_untracked(),
        super::loop_seam::SeamPhase::Idle
    )
}

fn slot_thumb_ready(pl: VideoPlaylistRef, global: u64) -> bool {
    let model = pl.model.get_untracked();
    let Some(pos) = model.at_global_frame(u32::try_from(global).unwrap_or(u32::MAX)) else {
        return false;
    };
    if let Some(video) = video_for_clip(pl, pos.clip_index) {
        return video.thumb_has_key(pos.local_frame);
    }
    false
}

fn video_for_clip(pl: VideoPlaylistRef, clip_index: u32) -> Option<crate::VideoRef> {
    if pl.live_clip.get_untracked() == clip_index {
        return Some(pl.live_video());
    }
    if pl.idle_clip.get_untracked() == Some(clip_index) {
        return Some(pl.idle_video());
    }
    None
}

/// Wire Pause progressive: Effect kick when Pause / slots ready (no ensure walk).
pub(crate) fn install_vt_thumb_effects(pl: VideoPlaylistRef) {
    Effect::new(move |_| {
        let ready = pl.ready.get();
        let playing = pl.playing.get();
        let scrubbing = pl.scrubbing.get();
        let _live = pl.live_clip.get();
        let _idle = pl.idle_clip.get();
        let _live_is_a = pl.live_is_a.get();
        let _seam = pl.seam_phase.get();
        let _a = pl.slot_a.ready.get();
        let _b = pl.slot_b.ready.get();
        if !ready || playing || scrubbing {
            return;
        }
        kick_vt_thumb_fill(pl);
    });
}

/// Clear sync-key cache on playlist close/open.
pub(crate) fn clear_thumb_sync_keys(pl: VideoPlaylistRef) {
    pl.thumb_sync_keys.set_value(HashMap::new());
    pl.thumb_fill_inflight.set_value(false);
    pl.thumb_fill_skip.set_value(HashSet::new());
    pl.thumb_clips_done.set_value(HashSet::new());
    if let Some(h) = pl.exact_ledger() {
        h.borrow_mut().clear_thumbs();
    }
}
