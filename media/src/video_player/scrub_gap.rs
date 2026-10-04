//! Densify gap targeting for [`super::VideoPlayer`].
//!
//! Residency queries stay here; pure pick logic is [`crate::scrub_plan`].

use crate::demux::Mp4VideoTrack;
use crate::scrub_plan::{decide_next_gap, gop_sample_lead_frames};
use crate::types::FrameId;

use super::Shared;

pub(super) fn playhead_and_last(shared: &Shared) -> (FrameId, FrameId) {
    let state = shared.player.borrow().state();
    let last = state.frame_count.saturating_sub(1);
    let committed = state.frame.min(last);
    let aimed = shared.scrub_chunks.borrow().playhead().min(last);
    // During scrub, aim follows the pointer; otherwise the committed playhead.
    let playhead = if aimed != committed { aimed } else { committed };
    (playhead, last)
}

/// Next missing frame nearest the playhead (biased by scrub `dir`), or `None`
/// when saturated / disarmed / stalled at this aim.
pub(super) fn next_gap_target(shared: &Shared) -> Option<FrameId> {
    let (playhead, last) = playhead_and_last(shared);
    // fMP4 warm open: provisional duration may exceed indexed samples — never
    // densify past the indexed prefix (would stall on mfra backfill + decode).
    let indexed_last = shared
        .player
        .borrow()
        .track()
        .indexed_frame_count()
        .saturating_sub(1);
    let last_clamped = last.min(indexed_last);
    let playhead_clamped = playhead.min(last_clamped);
    let dir = shared.scrub_dir.get();
    let policy = shared.player.borrow().metadata().scrub_policy();
    let (margin_left, margin_right) = (policy.margin_left, policy.margin_right);
    // Play keeps markers and trims Exact (not a full wipe) — ImageBitmap holes
    // must still win over marker `no_gaps`, or pause densify never re-warms Exact.
    let blit_gap = shared.scrub_blit.borrow().next_gap_frame(
        playhead_clamped,
        last_clamped,
        dir,
        margin_left,
        margin_right,
    );
    let (marker_saturated, marker_armed, marker_gap) = {
        let cache = shared.scrub_chunks.borrow();
        (
            cache.gaps_saturated(playhead_clamped, last_clamped, dir),
            cache.gap_fill_armed(playhead_clamped, last_clamped, margin_left, margin_right),
            cache.next_gap_frame(playhead_clamped, last_clamped, dir),
        )
    };
    let decision = decide_next_gap(
        playhead,
        last,
        indexed_last,
        shared.fill_stalled_aim.get(),
        blit_gap,
        marker_saturated,
        marker_armed,
        marker_gap,
    );
    if decision.clear_fill_stalled_aim {
        shared.fill_stalled_aim.set(None);
    }
    decision.target
}

/// Frames to warm for Exact settle / PlayPrime: one GOP at `key`, clamped by
/// [`crate::SettlePolicy`] (not a fixed 24).
pub(super) fn gop_sample_lead(track: &Mp4VideoTrack, key: FrameId) -> u64 {
    let n = track
        .index()
        .gop_frame_ids(key)
        .map(|ids| ids.len() as u64)
        .unwrap_or(12);
    gop_sample_lead_frames(n)
}
