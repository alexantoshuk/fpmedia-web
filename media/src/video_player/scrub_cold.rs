//! Cold scrub settle / byte-warm gates for [`super::VideoPlayer`].
//!
//! Soft path when Exact would stampede cold Ranges. Commit rule:
//! [`crate::scrub_plan::cold_settle_commit_frame`].
//! `mediabunny: src/media-sink.ts` `mediaSamplesAtTimestamps` (async retrieve).

use std::rc::Rc;

use crate::decode::scrub_blit::draw_image_bitmap;
use crate::error::MediaError;
use crate::scrub_plan::cold_settle_commit_frame;
use crate::types::FrameId;

use super::Shared;
use super::VideoPlayer;

/// True when every sample in the GOP at `key` is indexed and byte-ready.
///
/// **Url:** resident in [`crate::FetchMap`] (CachedSource windows) — cold → densify
/// must prefetch, not decode. **Blob/Memory:** JS already holds the file
/// (`mediabunny: BlobSource`); skip FetchMap — want-only WASM mirrors must not
/// block densify / punch the gray strip via `mark_full`.
pub(super) fn scrub_gop_bytes_warm(shared: &Shared, key: FrameId) -> bool {
    let track = shared.player.borrow().track_rc();
    let indexed = track.indexed_frame_count();
    if key >= indexed {
        return false;
    }
    let index = track.index();
    let ids = index.gop_frame_ids(key).unwrap_or_default();
    if ids.is_empty() {
        return false;
    }
    if ids.iter().any(|&id| id >= indexed) {
        return false;
    }
    // Local Blob/Memory — samples are always sliceable; FetchMap only mirrors
    // the tiny WASM cache for the gray meter.
    if !track.source().network_min_load() {
        return true;
    }
    for id in ids {
        let Some(entry) = index.get(id) else {
            return false;
        };
        let end = entry.offset.saturating_add(entry.size);
        if !shared.fetch_map.covers_range(entry.offset, end) {
            return false;
        }
    }
    true
}

/// Settle/seek may run Exact cascade only when aim GOP samples are indexed + resident.
///
/// Cold → soft present + prefetch ([`soft_cold_scrub_settle`]); densify paints later.
pub(super) fn exact_scrub_ready(shared: &Shared, frame: FrameId) -> bool {
    let track = shared.player.borrow().track_rc();
    let indexed = track.indexed_frame_count();
    if frame >= indexed {
        return false;
    }
    let key = track.index().nearest_keyframe(frame).unwrap_or(frame);
    scrub_gop_bytes_warm(shared, key)
}

/// Soft settle/seek on a cold frame: prefetch only, optional warm blit, commit
/// only within the **indexed** prefix.
///
/// **Named recovery** (not a [`crate::SettlePolicy`] knob). Does **not** decode.
pub(super) fn soft_cold_scrub_settle(
    player: &VideoPlayer,
    frame: FrameId,
) -> Result<(), MediaError> {
    let indexed = player.shared.player.borrow().track().indexed_frame_count();
    let key = player.key_for(frame).unwrap_or(frame);
    warm_scrub_gop_bytes(&player.shared, key);
    // Soft present: nearest warm blit only — never cold decode on the OpQueue.
    if let Some(preview) = player.scrub_preview_frame(frame) {
        let _ = player.try_present_warm_blit(preview)?;
    }
    if let Some(commit) = cold_settle_commit_frame(frame, indexed) {
        player.shared.player.borrow_mut().commit_frame(commit)?;
        player.touch_playhead(commit);
    }
    Ok(())
}

/// When soft cold settle deferred Exact and densify now has the aim blit, paint it.
pub(super) fn try_resolve_pending_cold_exact(shared: &Rc<Shared>) {
    let Some(aim) = shared.pending_cold_exact.get() else {
        return;
    };
    if !exact_scrub_ready(shared, aim) {
        return;
    }
    let Ok(mut blit) = shared.scrub_blit.try_borrow_mut() else {
        return;
    };
    let Some(bitmap) = blit.get(aim) else {
        return;
    };
    if draw_image_bitmap(&shared.canvas, &shared.ctx, bitmap).is_ok() {
        let _ = shared.player.borrow_mut().commit_frame(aim);
        shared.pending_cold_exact.set(None);
    }
}

/// Kick sample prefetch (and fragment backfill if needed) for a cold densify GOP.
pub(super) fn warm_scrub_gop_bytes(shared: &Shared, key: FrameId) {
    let track = shared.player.borrow().track_rc();
    let indexed = track.indexed_frame_count();
    if key >= indexed {
        track.kick_fragment_backfill();
        return;
    }
    let end = track
        .index()
        .gop_frame_ids(key)
        .unwrap_or_default()
        .last()
        .copied()
        .unwrap_or(key)
        .saturating_add(1)
        .min(indexed);
    track.kick_prefetch_samples(key as usize, end as usize);
}
