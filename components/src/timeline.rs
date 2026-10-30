//! Shared scrub / hover / strip timeline handle — [`crate::Video`] or [`crate::VideoPlaylist`].
//!
//! Gesture helpers ([`crate::Scrubber`], [`crate::HoverThumb`], [`crate::TimelineMeta`])
//! take [`TimelineRef`] as the scrub **domain**. Pipeline strips follow
//! [`Self::strip_video`] (playlist → live slot); ready/fetch marks are remapped into
//! timeline units here — Meta does not special-case playlist.
//! Design: `.ai/architecture.md` § Playlist composition; UX: `.ai/product.md`.

use web_sys::HtmlCanvasElement;

use leptos::prelude::*;

use crate::ReadyFrameMark;
use crate::VideoPlaylistRef;
use crate::VideoRef;
use crate::video_playlist::PlaylistPos;
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

/// Identity of [`TimelineRef::strip_video`] for pipeline interest re-lease.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum StripVideoKey {
    /// Single-clip timeline.
    Video,
    /// Playlist live slot (`true` = slot A).
    PlaylistLive { live_is_a: bool },
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

    /// Underlying playlist when this timeline is a [`VideoPlaylistRef`] view (units = ms).
    pub fn playlist(self) -> Option<VideoPlaylistRef> {
        match self.inner {
            TimelineInner::Playlist(p) => Some(p),
            TimelineInner::Video(_) => None,
        }
    }

    /// Video whose `pipeline` feeds timeline strips / meta.
    ///
    /// Tracks playlist `live_is_a` when composed. Hosts must **not** remount
    /// `<Scrubber>` when this flips — Meta re-leases via [`Self::strip_video_key`].
    pub fn strip_video(self) -> Option<VideoRef> {
        match self.inner {
            TimelineInner::Video(v) => Some(v),
            TimelineInner::Playlist(p) => Some(if p.live_is_a.get() {
                p.slot_a
            } else {
                p.slot_b
            }),
        }
    }

    pub(crate) fn strip_video_key(self) -> Option<StripVideoKey> {
        match self.inner {
            TimelineInner::Video(_) => Some(StripVideoKey::Video),
            TimelineInner::Playlist(p) => Some(StripVideoKey::PlaylistLive {
                live_is_a: p.live_is_a.get(),
            }),
        }
    }

    /// Domain length for strip `f/n` (same as tip / gesture `frame_count`).
    pub fn strip_domain_len(self) -> f64 {
        self.frame_count.get().max(1) as f64
    }

    /// Live pipeline ready marks → timeline units (identity for single-clip;
    /// playlist remaps local FrameId → ms in the live clip window).
    pub fn strip_ready_marks(self, marks: &[ReadyFrameMark]) -> Vec<ReadyFrameMark> {
        match self.inner {
            TimelineInner::Video(_) => marks.to_vec(),
            TimelineInner::Playlist(p) => {
                let live_clip = p.live_clip.get();
                let model = p.model.get();
                marks
                    .iter()
                    .filter_map(|m| {
                        let lo_f = u32::try_from(m.frame).ok()?;
                        let hi_f = u32::try_from(m.end).ok()?;
                        let lo = model.ms_at(PlaylistPos::new(live_clip, lo_f))? as u64;
                        let hi = model.ms_at(PlaylistPos::new(live_clip, hi_f))? as u64;
                        Some(ReadyFrameMark::run(lo, hi, m.kind))
                    })
                    .collect()
            }
        }
    }

    /// Live FetchMap file fractions → timeline bar fractions (playlist: clip window).
    pub fn strip_fetch_ranges(self, ranges: &[(f32, f32)]) -> Vec<(f32, f32)> {
        match self.inner {
            TimelineInner::Video(_) => ranges.to_vec(),
            TimelineInner::Playlist(p) => {
                let live_clip = p.live_clip.get();
                let model = p.model.get();
                let total_ms = self.frame_count.get().max(1);
                let start = model.clip_start_ms(live_clip).unwrap_or(0);
                let end = model.clip_end_ms(live_clip).unwrap_or(total_ms);
                fetch_ranges_in_clip_window(ranges, start, end, total_ms)
            }
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

fn fetch_ranges_in_clip_window(
    ranges: &[(f32, f32)],
    clip_start_ms: u32,
    clip_end_ms: u32,
    total_ms: u32,
) -> Vec<(f32, f32)> {
    if ranges.is_empty() || total_ms == 0 || clip_end_ms <= clip_start_ms {
        return Vec::new();
    }
    let t = f64::from(total_ms);
    let origin = f64::from(clip_start_ms) / t;
    let span = f64::from(clip_end_ms.saturating_sub(clip_start_ms)) / t;
    ranges
        .iter()
        .map(|&(a, b)| {
            let a = (origin + f64::from(a).clamp(0.0, 1.0) * span) as f32;
            let b = (origin + f64::from(b).clamp(0.0, 1.0) * span) as f32;
            (a.min(b), a.max(b))
        })
        .collect()
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
    /// Scrubber / hover / strip surface for this element (shared with VideoPlaylist).
    pub fn as_timeline(self) -> TimelineRef {
        TimelineRef::from_video(self)
    }
}

impl VideoPlaylistRef {
    /// Duration-weighted scrub/hover/strip surface (units = milliseconds).
    pub fn as_timeline(self) -> TimelineRef {
        TimelineRef::from_playlist(self)
    }
}
