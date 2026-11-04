//! Playlist loop ownership + seam phase — **no equivalent** (host local).
//!
//! Design: `.ai/architecture.md` § Playlist composition / State machines.
//! Vocab: `.ai/glossary.md` § LoopOwner / SeamPhase.

use leptos::prelude::*;

use super::video_playlist_ref::VideoPlaylistRef;

/// Who arms LoopZone wrap for the current playlist zone vs live clip.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LoopOwner {
    /// Playlist `looping` off.
    Off,
    /// Zone ⊆ live clip → engine `LoopSeam` on the live `<Video>`.
    Engine,
    /// Cross-clip / multi-clip wall → host `promote_idle_at` / `wrap_to_loop_in`.
    Host,
}

/// Host playlist seam in flight (cut promote or LoopZone wrap).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub(crate) enum SeamPhase {
    #[default]
    Idle,
    Preparing,
    Swapping,
    Playing,
}

impl SeamPhase {
    pub(crate) fn is_busy(self) -> bool {
        !matches!(self, Self::Idle)
    }
}

/// Derive ownership from playlist `looping` + global zone vs live clip span.
/// Does **not** read slot `Video.looping` (that is a sync side-effect).
pub(crate) fn loop_owner(pl: VideoPlaylistRef) -> LoopOwner {
    if !pl.looping.get_untracked() {
        return LoopOwner::Off;
    }
    let model = pl.model.get_untracked();
    let live_i = pl.live_clip.get_untracked();
    let (zone_in, zone_out) = pl.loop_zone();
    let Some(clip_start) = model.clip_start_frame(live_i) else {
        return LoopOwner::Host;
    };
    let Some(clip_end) = model.clip_end_frame(live_i) else {
        return LoopOwner::Host;
    };
    if zone_in >= clip_start && zone_out <= clip_end && zone_out > zone_in {
        LoopOwner::Engine
    } else {
        LoopOwner::Host
    }
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
    if pl.seam_phase.get_untracked().is_busy() {
        return false;
    }
    set_seam_phase(pl, SeamPhase::Preparing);
    true
}

/// Begin if Idle; stay if already Preparing; reject Swapping/Playing.
pub(crate) fn ensure_seam_preparing(pl: VideoPlaylistRef) -> bool {
    match pl.seam_phase.get_untracked() {
        SeamPhase::Idle => try_begin_seam(pl),
        SeamPhase::Preparing => true,
        SeamPhase::Swapping | SeamPhase::Playing => false,
    }
}

pub(crate) fn end_seam(pl: VideoPlaylistRef) {
    set_seam_phase(pl, SeamPhase::Idle);
}
