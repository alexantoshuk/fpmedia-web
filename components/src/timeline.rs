//! Shared scrub / hover timeline handle — single [`crate::Video`] or [`crate::VideoPlaylist`].
//!
//! Gesture helpers ([`crate::Scrubber`], [`crate::HoverThumb`]) take [`TimelineRef`].
//! Strip meta that needs `pipeline` still binds an optional [`crate::VideoRef`].
//! Design: `.ai/architecture.md` § Playlist composition; UX: `.ai/product.md`.

use web_sys::HtmlCanvasElement;

use leptos::prelude::*;

use crate::VideoPlaylistRef;
use crate::VideoRef;
use crate::video_playlist::video_playlist_ref::ScrubPhase;

/// Scrub-domain handle: `playhead` / `frame_count` are discrete timeline units
/// ([`VideoRef`]: presentation frames; [`VideoPlaylistRef`]: playlist milliseconds).
#[derive(Clone, Copy)]
pub struct TimelineRef {
    pub ready: RwSignal<bool>,
    pub playhead: RwSignal<u32>,
    pub frame_count: RwSignal<u32>,
    inner: TimelineInner,
}

#[derive(Clone, Copy)]
#[allow(clippy::large_enum_variant)] // Copy handle; VideoPlaylistRef carries two VideoRefs by design
enum TimelineInner {
    Video(VideoRef),
    Playlist(VideoPlaylistRef),
}

impl TimelineRef {
    /// View onto a mounted [`VideoRef`] (same signals + scrub/hover ops).
    pub fn from_video(video: VideoRef) -> Self {
        Self {
            ready: video.ready,
            playhead: video.playhead,
            frame_count: video.frame_count,
            inner: TimelineInner::Video(video),
        }
    }

    /// View onto a mounted [`VideoPlaylistRef`] — units are playlist milliseconds.
    pub fn from_playlist(playlist: VideoPlaylistRef) -> Self {
        Self {
            ready: playlist.ready,
            playhead: playlist.playhead_ms,
            frame_count: playlist.duration_ms,
            inner: TimelineInner::Playlist(playlist),
        }
    }

    /// Underlying single-clip video when this timeline is a [`VideoRef`] view.
    pub fn video(self) -> Option<VideoRef> {
        match self.inner {
            TimelineInner::Video(v) => Some(v),
            TimelineInner::Playlist(_) => None,
        }
    }

    pub fn scrub_pointer_down(self, unit: u32) {
        match self.inner {
            TimelineInner::Video(v) => v.scrub_pointer_down(unit),
            TimelineInner::Playlist(p) => p.scrub_timeline(unit, ScrubPhase::Down),
        }
    }

    pub fn scrub_pointer_move(self, unit: u32) {
        match self.inner {
            TimelineInner::Video(v) => v.scrub_pointer_move(unit),
            TimelineInner::Playlist(p) => p.scrub_timeline(unit, ScrubPhase::Move),
        }
    }

    pub fn scrub_pointer_up(self, unit: u32) {
        match self.inner {
            TimelineInner::Video(v) => v.scrub_pointer_up(unit),
            TimelineInner::Playlist(p) => p.scrub_timeline(unit, ScrubPhase::Up),
        }
    }

    pub fn paint_hover_thumb(self, unit: u32, dest: HtmlCanvasElement, max_w: u32, max_h: u32) {
        match self.inner {
            TimelineInner::Video(v) => v.paint_hover_thumb(unit, dest, max_w, max_h),
            TimelineInner::Playlist(p) => p.paint_hover_thumb(unit, dest, max_w, max_h),
        }
    }

    pub fn try_paint_hover_thumb(
        self,
        unit: u32,
        dest: HtmlCanvasElement,
        max_w: u32,
        max_h: u32,
    ) -> bool {
        match self.inner {
            TimelineInner::Video(v) => v.try_paint_hover_thumb(unit, dest, max_w, max_h),
            TimelineInner::Playlist(p) => p.try_paint_hover_thumb(unit, dest, max_w, max_h),
        }
    }

    pub fn cancel_hover_thumb(self) {
        match self.inner {
            TimelineInner::Video(v) => v.cancel_hover_thumb(),
            TimelineInner::Playlist(p) => p.cancel_hover_thumb(),
        }
    }
}

impl From<VideoRef> for TimelineRef {
    fn from(video: VideoRef) -> Self {
        Self::from_video(video)
    }
}

impl From<VideoPlaylistRef> for TimelineRef {
    fn from(playlist: VideoPlaylistRef) -> Self {
        Self::from_playlist(playlist)
    }
}

impl VideoRef {
    /// Scrubber / hover surface for this element (shared with VideoPlaylist).
    pub fn as_timeline(self) -> TimelineRef {
        TimelineRef::from_video(self)
    }
}

impl VideoPlaylistRef {
    /// Duration-weighted scrub/hover surface (units = milliseconds).
    pub fn as_timeline(self) -> TimelineRef {
        TimelineRef::from_playlist(self)
    }
}
