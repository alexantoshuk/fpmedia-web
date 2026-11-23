//! Shared transport facade — [`crate::Video`] or [`crate::VideoPlaylist`].
//!
//! Host chrome uses [`MediaRef`] for play/pause/seek/goto/step/loop/gain.
//! Scrub/hover/strip stay on [`crate::TimelineRef`] via [`Self::as_timeline`].
//! Open, `[i]` / [`crate::PlaylistPos`], and slot/engine knobs stay on the
//! concrete refs.
//! Design: `.ai/architecture.md` § Virtual Timeline; UX: `.ai/product.md`.

use fpmedia_web::LoopZone;
use leptos::prelude::*;

use crate::TimelineRef;
use crate::VideoPlaylistRef;
use crate::VideoRef;

/// Shared transport ops implemented by [`VideoRef`] and [`VideoPlaylistRef`].
///
/// Keeps [`MediaRef`] forwards signature-aligned; public API stays the enum facade.
pub(crate) trait MediaTransport: Copy {
    fn play(self);
    fn pause(self);
    fn close(self);
    fn seek_frame(self, frame: u32);
    fn goto_frame(self, frame: u32);
    fn next_frame(self);
    fn previous_frame(self);
    fn set_loop(self, on: bool);
    fn loop_zone(self) -> LoopZone;
    fn set_loop_zone(self, in_frame: u32, out_frame: u32);
    fn set_volume(self, volume: f64);
    fn set_muted(self, muted: bool);
    fn set_playback_rate(self, rate: f64);
    fn thumb_max_size(self) -> (u32, u32);
    fn set_thumb_max_size(self, max_w: u32, max_h: u32);
}

impl MediaTransport for VideoRef {
    fn play(self) {
        VideoRef::play(&self);
    }
    fn pause(self) {
        VideoRef::pause(&self);
    }
    fn close(self) {
        VideoRef::close(&self);
    }
    fn seek_frame(self, frame: u32) {
        VideoRef::seek_frame(&self, frame);
    }
    fn goto_frame(self, frame: u32) {
        VideoRef::goto_frame(&self, frame);
    }
    fn next_frame(self) {
        VideoRef::next_frame(&self);
    }
    fn previous_frame(self) {
        VideoRef::previous_frame(&self);
    }
    fn set_loop(self, on: bool) {
        VideoRef::set_loop(&self, on);
    }
    fn loop_zone(self) -> LoopZone {
        VideoRef::loop_zone(&self)
    }
    fn set_loop_zone(self, in_frame: u32, out_frame: u32) {
        VideoRef::set_loop_zone(&self, in_frame, out_frame);
    }
    fn set_volume(self, volume: f64) {
        VideoRef::set_volume(&self, volume);
    }
    fn set_muted(self, muted: bool) {
        VideoRef::set_muted(&self, muted);
    }
    fn set_playback_rate(self, rate: f64) {
        VideoRef::set_playback_rate(&self, rate);
    }
    fn thumb_max_size(self) -> (u32, u32) {
        VideoRef::thumb_max_size(&self)
    }
    fn set_thumb_max_size(self, max_w: u32, max_h: u32) {
        VideoRef::set_thumb_max_size(&self, max_w, max_h);
    }
}

impl MediaTransport for VideoPlaylistRef {
    fn play(self) {
        VideoPlaylistRef::play(self);
    }
    fn pause(self) {
        VideoPlaylistRef::pause(self);
    }
    fn close(self) {
        VideoPlaylistRef::close(self);
    }
    fn seek_frame(self, frame: u32) {
        VideoPlaylistRef::seek_frame(self, frame);
    }
    fn goto_frame(self, frame: u32) {
        VideoPlaylistRef::goto_frame(self, frame);
    }
    fn next_frame(self) {
        VideoPlaylistRef::next_frame(self);
    }
    fn previous_frame(self) {
        VideoPlaylistRef::previous_frame(self);
    }
    fn set_loop(self, on: bool) {
        VideoPlaylistRef::set_loop(self, on);
    }
    fn loop_zone(self) -> LoopZone {
        VideoPlaylistRef::loop_zone(self)
    }
    fn set_loop_zone(self, in_frame: u32, out_frame: u32) {
        VideoPlaylistRef::set_loop_zone(self, in_frame, out_frame);
    }
    fn set_volume(self, volume: f64) {
        VideoPlaylistRef::set_volume(self, volume);
    }
    fn set_muted(self, muted: bool) {
        VideoPlaylistRef::set_muted(self, muted);
    }
    fn set_playback_rate(self, rate: f64) {
        VideoPlaylistRef::set_playback_rate(self, rate);
    }
    fn thumb_max_size(self) -> (u32, u32) {
        VideoPlaylistRef::thumb_max_size(self)
    }
    fn set_thumb_max_size(self, max_w: u32, max_h: u32) {
        VideoPlaylistRef::set_thumb_max_size(self, max_w, max_h);
    }
}

/// Expand `MediaRef` transport methods as one `match` + [`MediaTransport`] call.
macro_rules! media_fwd {
    ($(
        $(#[$meta:meta])*
        $vis:vis fn $name:ident(self $(, $arg:ident: $ty:ty)*) $(-> $ret:ty)?;
    )+) => {$(
        $(#[$meta])*
        $vis fn $name(self $(, $arg: $ty)*) $(-> $ret)? {
            match self.inner {
                MediaInner::Video(v) => MediaTransport::$name(v $(, $arg)*),
                MediaInner::Playlist(p) => MediaTransport::$name(p $(, $arg)*),
            }
        }
    )+};
}

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
            MediaInner::Playlist(p) => p.live_video_tracked(),
        }
    }

    /// Scrub/hover/strip surface (timeline FrameId units).
    pub fn as_timeline(self) -> TimelineRef {
        match self.inner {
            MediaInner::Video(v) => TimelineRef::from_video(v),
            MediaInner::Playlist(p) => TimelineRef::from_playlist(p),
        }
    }

    media_fwd! {
        pub fn play(self);
        pub fn pause(self);
        pub fn close(self);
        /// Relocate on the timeline FrameId axis (internal). Prefer [`Self::goto_frame`] for UI.
        pub fn seek_frame(self, frame: u32);
        /// User goto (timeline FrameId): stop Play intent, ignore LoopZone.
        pub fn goto_frame(self, frame: u32);
        pub fn next_frame(self);
        pub fn previous_frame(self);
        pub fn set_loop(self, on: bool);
        /// Resolved half-open LoopZone in timeline frames.
        pub fn loop_zone(self) -> LoopZone;
        /// Set half-open `[in, out)` in timeline FrameIds.
        pub fn set_loop_zone(self, in_frame: u32, out_frame: u32);
        pub fn set_volume(self, volume: f64);
        pub fn set_muted(self, muted: bool);
        pub fn set_playback_rate(self, rate: f64);
        /// Thumb decode max box — see [`crate::VideoRef::thumb_max_size`].
        pub fn thumb_max_size(self) -> (u32, u32);
        /// Set thumb decode max box (hover + Pause progressive). Clears thumb caches.
        pub fn set_thumb_max_size(self, max_w: u32, max_h: u32);
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
