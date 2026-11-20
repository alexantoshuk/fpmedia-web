//! Shared transport facade — [`crate::Video`] or [`crate::VideoPlaylist`].
//!
//! Host chrome uses [`MediaRef`] for play/pause/seek/goto/step/loop/gain.
//! Scrub/hover/strip stay on [`crate::TimelineRef`] via [`Self::as_timeline`].
//! Open, `[i]` / [`crate::PlaylistPos`], and slot/engine knobs stay on the
//! concrete refs.
//! Design: `.ai/architecture.md` § Virtual Timeline; UX: `.ai/product.md`.

use leptos::prelude::*;

use crate::TimelineRef;
use crate::VideoPlaylistRef;
use crate::VideoRef;

/// Transport-domain handle over a mounted [`VideoRef`] or [`VideoPlaylistRef`].
///
/// Shared signals are aliased from the backend (same pattern as [`TimelineRef`]).
/// Frame units for seek/goto/loop = timeline FrameId (single = identity; playlist = Σ).
#[derive(Clone, Copy)]
pub struct MediaRef {
    pub ready: RwSignal<bool>,
    pub busy: RwSignal<bool>,
    pub playing: RwSignal<bool>,
    pub ended: RwSignal<bool>,
    pub looping: RwSignal<bool>,
    pub loop_in: RwSignal<u32>,
    pub loop_out: RwSignal<Option<u32>>,
    pub playhead: RwSignal<u32>,
    pub frame_count: RwSignal<u32>,
    pub volume: RwSignal<f64>,
    pub muted: RwSignal<bool>,
    pub playback_rate: RwSignal<f64>,
    pub status: RwSignal<String>,
    pub error: RwSignal<Option<String>>,
    pub exact_residency_epoch: RwSignal<u64>,
    inner: MediaInner,
}

#[derive(Clone, Copy)]
#[allow(clippy::large_enum_variant)] // Copy handle; VideoPlaylistRef carries two VideoRefs by design
enum MediaInner {
    Video(VideoRef),
    Playlist(VideoPlaylistRef),
}

impl MediaRef {
    /// View onto a mounted [`VideoRef`].
    pub fn from_video(video: VideoRef) -> Self {
        Self {
            ready: video.ready,
            busy: video.busy,
            playing: video.playing,
            ended: video.ended,
            looping: video.looping,
            loop_in: video.loop_in,
            loop_out: video.loop_out,
            playhead: video.playhead,
            frame_count: video.frame_count,
            volume: video.volume,
            muted: video.muted,
            playback_rate: video.playback_rate,
            status: video.status,
            error: video.error,
            exact_residency_epoch: video.exact_residency_epoch,
            inner: MediaInner::Video(video),
        }
    }

    /// View onto a mounted [`VideoPlaylistRef`] (Σ-frame timeline).
    pub fn from_playlist(playlist: VideoPlaylistRef) -> Self {
        Self {
            ready: playlist.ready,
            busy: playlist.busy,
            playing: playlist.playing,
            ended: playlist.ended,
            looping: playlist.looping,
            loop_in: playlist.loop_in,
            loop_out: playlist.loop_out,
            playhead: playlist.playhead,
            frame_count: playlist.frame_count,
            volume: playlist.volume,
            muted: playlist.muted,
            playback_rate: playlist.playback_rate,
            status: playlist.status,
            error: playlist.error,
            exact_residency_epoch: playlist.exact_residency_epoch,
            inner: MediaInner::Playlist(playlist),
        }
    }

    /// Underlying single-clip video when this is a [`VideoRef`] view.
    pub fn video(self) -> Option<VideoRef> {
        match self.inner {
            MediaInner::Video(v) => Some(v),
            MediaInner::Playlist(_) => None,
        }
    }

    /// Underlying playlist when this is a [`VideoPlaylistRef`] view.
    pub fn playlist(self) -> Option<VideoPlaylistRef> {
        match self.inner {
            MediaInner::Playlist(p) => Some(p),
            MediaInner::Video(_) => None,
        }
    }

    /// Live paint/decode slot: single video, or playlist live slot.
    ///
    /// Tracks playlist `live_is_a` when composed (reactive).
    pub fn active_video(self) -> VideoRef {
        match self.inner {
            MediaInner::Video(v) => v,
            MediaInner::Playlist(p) => {
                if p.live_is_a.get() {
                    p.slot_a
                } else {
                    p.slot_b
                }
            }
        }
    }

    /// Scrub/hover/strip surface (timeline FrameId units).
    pub fn as_timeline(self) -> TimelineRef {
        match self.inner {
            MediaInner::Video(v) => TimelineRef::from_video(v),
            MediaInner::Playlist(p) => TimelineRef::from_playlist(p),
        }
    }

    pub fn play(self) {
        match self.inner {
            MediaInner::Video(v) => v.play(),
            MediaInner::Playlist(p) => p.play(),
        }
    }

    pub fn pause(self) {
        match self.inner {
            MediaInner::Video(v) => v.pause(),
            MediaInner::Playlist(p) => p.pause(),
        }
    }

    pub fn close(self) {
        match self.inner {
            MediaInner::Video(v) => v.close(),
            MediaInner::Playlist(p) => p.close(),
        }
    }

    /// Relocate on the timeline FrameId axis (internal). Prefer [`Self::goto_frame`] for UI.
    pub fn seek_frame(self, frame: u32) {
        match self.inner {
            MediaInner::Video(v) => v.seek_frame(frame),
            MediaInner::Playlist(p) => p.seek_frame(frame),
        }
    }

    /// User goto (timeline FrameId): stop Play intent, ignore LoopZone.
    pub fn goto_frame(self, frame: u32) {
        match self.inner {
            MediaInner::Video(v) => v.goto_frame(frame),
            MediaInner::Playlist(p) => p.goto_frame(frame),
        }
    }

    pub fn next_frame(self) {
        match self.inner {
            MediaInner::Video(v) => v.next_frame(),
            MediaInner::Playlist(p) => p.next_frame(),
        }
    }

    pub fn previous_frame(self) {
        match self.inner {
            MediaInner::Video(v) => v.previous_frame(),
            MediaInner::Playlist(p) => p.previous_frame(),
        }
    }

    pub fn set_loop(self, on: bool) {
        match self.inner {
            MediaInner::Video(v) => v.set_loop(on),
            MediaInner::Playlist(p) => p.set_loop(on),
        }
    }

    /// Resolved half-open LoopZone in timeline frames.
    pub fn loop_zone(self) -> (u32, u32) {
        match self.inner {
            MediaInner::Video(v) => v.loop_zone(),
            MediaInner::Playlist(p) => p.loop_zone(),
        }
    }

    /// Set half-open `[in, out)` in timeline FrameIds.
    pub fn set_loop_zone(self, in_frame: u32, out_frame: u32) {
        match self.inner {
            MediaInner::Video(v) => v.set_loop_zone(in_frame, out_frame),
            MediaInner::Playlist(p) => p.set_loop_zone(in_frame, out_frame),
        }
    }

    pub fn set_volume(self, volume: f64) {
        match self.inner {
            MediaInner::Video(v) => v.set_volume(volume),
            MediaInner::Playlist(p) => p.set_volume(volume),
        }
    }

    pub fn set_muted(self, muted: bool) {
        match self.inner {
            MediaInner::Video(v) => v.set_muted(muted),
            MediaInner::Playlist(p) => p.set_muted(muted),
        }
    }

    pub fn set_playback_rate(self, rate: f64) {
        match self.inner {
            MediaInner::Video(v) => v.set_playback_rate(rate),
            MediaInner::Playlist(p) => p.set_playback_rate(rate),
        }
    }

    /// Thumb decode max box — see [`crate::VideoRef::thumb_max_size`].
    pub fn thumb_max_size(self) -> (u32, u32) {
        match self.inner {
            MediaInner::Video(v) => v.thumb_max_size(),
            MediaInner::Playlist(p) => p.thumb_max_size(),
        }
    }

    /// Set thumb decode max box (hover + Pause progressive). Clears thumb caches.
    pub fn set_thumb_max_size(self, max_w: u32, max_h: u32) {
        match self.inner {
            MediaInner::Video(v) => v.set_thumb_max_size(max_w, max_h),
            MediaInner::Playlist(p) => p.set_thumb_max_size(max_w, max_h),
        }
    }
}

impl From<VideoRef> for MediaRef {
    fn from(video: VideoRef) -> Self {
        Self::from_video(video)
    }
}

impl From<VideoPlaylistRef> for MediaRef {
    fn from(playlist: VideoPlaylistRef) -> Self {
        Self::from_playlist(playlist)
    }
}

impl VideoRef {
    /// Transport facade for this element (shared with VideoPlaylist).
    pub fn as_media(self) -> MediaRef {
        MediaRef::from_video(self)
    }
}

impl VideoPlaylistRef {
    /// Transport facade for this playlist (shared with Video).
    pub fn as_media(self) -> MediaRef {
        MediaRef::from_playlist(self)
    }
}
