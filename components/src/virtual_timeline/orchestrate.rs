//! VT densify kick: plan (timeline) → resolve → queue local target on a slot.
//!
//! Engine fill task still runs PreferSoftware decode; gap pick is VT-only when
//! [`fpmedia_web::VideoPlayer::set_vt_densify_authority`] is on.
//! **no equivalent** (VT local).

use std::collections::BTreeSet;

use fpmedia_web::FrameId;

use super::{
    BackendAim, DensifyPlanInput, ExactLedger, plan_next_densify_gap, resolve_playlist,
    resolve_single,
};
use crate::video_playlist::PlaylistModel;

/// Plan next timeline densify gap from ledger, or `None` when idle/saturated.
///
/// `skip_exact`: timeline FrameIds to ignore as Exact holes (VT soft-ban).
pub fn plan_timeline_densify_gap<V>(
    ledger: &ExactLedger<V>,
    aim: FrameId,
    timeline_last: FrameId,
    margin_left: FrameId,
    margin_right: FrameId,
    dir: i8,
    skip_exact: BTreeSet<FrameId>,
) -> Option<FrameId> {
    let mut input =
        DensifyPlanInput::from_ledger(ledger, aim, timeline_last, margin_left, margin_right, dir);
    input.skip_exact = skip_exact;
    plan_next_densify_gap(&input).target
}

/// Single-clip: timeline id == local.
pub fn resolve_densify_single(global: FrameId, frame_count: FrameId) -> Option<BackendAim> {
    resolve_single(global, frame_count)
}

/// Playlist: timeline id → clip + local.
pub fn resolve_densify_playlist(model: &PlaylistModel, global: FrameId) -> Option<BackendAim> {
    resolve_playlist(model, global)
}
