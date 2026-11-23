//! Playlist loop ownership + seam phase — signal I/O over VT reduce.
//!
//! Pure FSM / owner geometry: [`crate::virtual_timeline::loop_zone`].
//! Async wrap / promote: [`super::loop_seam_exec`].
//! Design: `.ai/architecture.md` § Playlist composition / Virtual Timeline.
//! Vocab: `.ai/glossary.md` § LoopOwner / SeamPhase.
//! **no equivalent** (host local).

use leptos::prelude::*;

use crate::frame_axis::GlobalFrame;
use crate::virtual_timeline::{
    seam_ensure_preparing as vt_seam_ensure_preparing, seam_try_begin, zone_owner_from_spans,
};

use super::video_playlist_ref::VideoPlaylistRef;

/// Host LoopZone seam diagnosis — leave false unless debugging wrap/promote.
pub(crate) const LOOP_SEAM_TRACE: bool = false;

pub(crate) fn seam_trace(msg: &str) {
    if LOOP_SEAM_TRACE {
        web_sys::console::log_1(&format!("fpmedia playlist seam: {msg}").into());
    }
}

pub(crate) use crate::virtual_timeline::SeamPhase;
/// Alias of VT [`crate::virtual_timeline::ZoneOwner`] (playlist vocab).
pub(crate) use crate::virtual_timeline::ZoneOwner as LoopOwner;

/// Derive ownership from playlist `looping` + global zone vs live clip span.
/// Does **not** read slot `Video.looping` (that is a sync side-effect).
pub(crate) fn loop_owner(pl: VideoPlaylistRef) -> LoopOwner {
    if !pl.looping.get_untracked() {
        return LoopOwner::Off;
    }
    let model = pl.model.get_untracked();
    let live_i = pl.live_clip.get_untracked();
    let zone = pl.loop_zone();
    let Some(clip_start) = model.clip_start_frame(live_i).map(GlobalFrame::get) else {
        return LoopOwner::Host;
    };
    let Some(clip_end) = model.clip_end_frame(live_i).map(GlobalFrame::get) else {
        return LoopOwner::Host;
    };
    zone_owner_from_spans(true, zone, clip_start, clip_end)
}

pub(crate) fn seam_phase(pl: VideoPlaylistRef) -> SeamPhase {
    pl.seam_phase.get_untracked()
}

pub(crate) fn seam_busy(pl: VideoPlaylistRef) -> bool {
    seam_phase(pl).is_busy()
}

pub(crate) fn set_seam_phase(pl: VideoPlaylistRef, phase: SeamPhase) {
    pl.seam_phase.set(phase);
}

/// Idle → Preparing. False if a seam is already in flight.
pub(crate) fn try_begin_seam(pl: VideoPlaylistRef) -> bool {
    let Some(next) = seam_try_begin(seam_phase(pl)) else {
        return false;
    };
    set_seam_phase(pl, next);
    true
}

/// Begin if Idle; stay if already Preparing; reject Swapping/Playing.
pub(crate) fn ensure_seam_preparing(pl: VideoPlaylistRef) -> bool {
    let Some(next) = vt_seam_ensure_preparing(seam_phase(pl)) else {
        return false;
    };
    set_seam_phase(pl, next);
    true
}

pub(crate) fn end_seam(pl: VideoPlaylistRef) {
    let g = pl.playhead.get_untracked();
    let zone_last = pl.loop_zone().out_frame.saturating_sub(1);
    seam_trace(&format!(
        "end_seam was={:?} playhead={g} zone_last={zone_last}",
        seam_phase(pl)
    ));
    set_seam_phase(pl, SeamPhase::Idle);
}
