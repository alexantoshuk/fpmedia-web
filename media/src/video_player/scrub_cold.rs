//! Cold scrub settle / byte-warm gates for [`super::VideoPlayer`].
//!
//! Soft path when Exact would stampede cold Ranges. Commit rule:
//! [`crate::scrub_plan::cold_settle_commit_frame`].
//! `mediabunny: src/media-sink.ts` `mediaSamplesAtTimestamps` (async retrieve).

use std::rc::Rc;

use crate::decode::scrub_blit::draw_image_bitmap;
use crate::error::MediaError;
use crate::scrub_plan::{cold_settle_commit_frame, next_fetchmap_gap};
use crate::types::FrameId;

use super::Shared;
use super::VideoPlayer;
use super::scrub_gap::playhead_and_last;
use super::sleep_ms;

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

/// Prefetch unread [`crate::FetchMap`] holes nearest the aim's file offset.
///
/// When green densify is saturated, gray file-byte gaps otherwise freeze forever
/// (hover/play only touch sample ranges they visit). Blob: `note_local_gray`
/// only. URL: bounded Range tiles. Returns `true` if any prefetch was kicked.
pub(super) fn kick_fetchmap_gap_fill(shared: &Shared) -> bool {
    if shared.file_size == 0 {
        return false;
    }
    let gaps = shared.fetch_map.gaps(shared.file_size);
    if gaps.is_empty() {
        return false;
    }
    let (playhead, _) = playhead_and_last(shared);
    let focus_offset = shared
        .player
        .borrow()
        .track()
        .index()
        .get(playhead)
        .map(|e| e.offset)
        .unwrap_or(0);
    let Some((gap_start, gap_end)) = next_fetchmap_gap(&gaps, focus_offset) else {
        return false;
    };
    let track = shared.player.borrow().track_rc();
    let source = track.source();
    // Blob: sync `note_local_gray` (quantum-sized). URL: small Range tiles so
    // we do not stampede OPEN_URL_PARALLELISM (mediabunny: BlobSource vs URL).
    let network = source.network_min_load();
    let gesture = shared.scrub_gesture.get();
    let tile = if network {
        (crate::source::GATE_LARGE_BYTES as u64) / 2
    } else {
        crate::source::DEFAULT_CACHE_SIZE as u64
    };
    // Idle: ≤2×quantum/tick. Under finger: 1 tile (hover-like trickle).
    let max_tiles = if network {
        4usize
    } else if gesture {
        1usize
    } else {
        2usize
    };
    let mut off = gap_start;
    let mut tiles = 0usize;
    while off < gap_end && tiles < max_tiles {
        let chunk = (gap_end - off).min(tile);
        if chunk == 0 {
            break;
        }
        source.prefetch(off, chunk as usize);
        off = off.saturating_add(chunk);
        tiles += 1;
    }
    tiles > 0
}

/// Arm independent gray hole-fill (does **not** latch [`Shared::scrub_busy`]).
///
/// Densify abort / Exact yield stay free; gray survives soft-scrub `abort_scrub_fill`.
/// mediabunny: no FetchMap meter — local plane only.
pub(super) fn schedule_fetchmap_gap_fill(shared: &Rc<Shared>) {
    if shared.is_disposed()
        || shared.engine.get().is_play()
        || shared.player.borrow().state().playing
        || shared.file_size == 0
        || shared.gray_fill_busy.get()
    {
        return;
    }
    if shared.fetch_map.gaps(shared.file_size).is_empty() {
        return;
    }
    shared.gray_fill_busy.set(true);
    let shared = Rc::clone(shared);
    wasm_bindgen_futures::spawn_local(async move {
        fetchmap_gap_fill_task(shared).await;
    });
}

async fn fetchmap_gap_fill_task(shared: Rc<Shared>) {
    while !shared.is_disposed()
        && !shared.engine.get().is_play()
        && !shared.player.borrow().state().playing
    {
        if !kick_fetchmap_gap_fill(&shared) {
            break;
        }
        // Idle: wall ~60 Hz. Drag: ~150 ms like hover coalesce (`DEFER_HOVER_PAINT_MS`)
        // so the red playhead stays free (paused rAF stalls; `sleep_ms(0)` blinks).
        let pace_ms = if shared.scrub_gesture.get() { 150 } else { 16 };
        sleep_ms(pace_ms).await;
    }
    shared.gray_fill_busy.set(false);
}
