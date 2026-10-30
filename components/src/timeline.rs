//! Shared scrub / hover timeline handle — single [`crate::Video`] or future Sequence.
//!
//! Gesture helpers ([`crate::Scrubber`], [`crate::HoverThumb`]) take [`TimelineRef`].
//! Strip meta that needs `pipeline` still binds an optional [`crate::VideoRef`].
//! Design: `.ai/architecture.md` § Sequence composition; UX: `.ai/product.md`.

use web_sys::HtmlCanvasElement;

use leptos::prelude::*;

use crate::VideoRef;

/// Scrub-domain handle: `playhead` / `frame_count` are discrete timeline units
/// (for [`VideoRef`]: presentation [`fpmedia_web`] frames).
#[derive(Clone, Copy)]
pub struct TimelineRef {
    pub ready: RwSignal<bool>,
    pub playhead: RwSignal<u32>,
    pub frame_count: RwSignal<u32>,
    inner: TimelineInner,
}

#[derive(Clone, Copy)]
enum TimelineInner {
    Video(VideoRef),
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

    /// Underlying single-clip video when this timeline is a [`VideoRef`] view.
    pub fn video(self) -> Option<VideoRef> {
        match self.inner {
            TimelineInner::Video(v) => Some(v),
        }
    }

    pub fn scrub_pointer_down(self, frame: u32) {
        match self.inner {
            TimelineInner::Video(v) => v.scrub_pointer_down(frame),
        }
    }

    pub fn scrub_pointer_move(self, frame: u32) {
        match self.inner {
            TimelineInner::Video(v) => v.scrub_pointer_move(frame),
        }
    }

    pub fn scrub_pointer_up(self, frame: u32) {
        match self.inner {
            TimelineInner::Video(v) => v.scrub_pointer_up(frame),
        }
    }

    pub fn paint_hover_thumb(self, frame: u32, dest: HtmlCanvasElement, max_w: u32, max_h: u32) {
        match self.inner {
            TimelineInner::Video(v) => v.paint_hover_thumb(frame, dest, max_w, max_h),
        }
    }

    pub fn try_paint_hover_thumb(
        self,
        frame: u32,
        dest: HtmlCanvasElement,
        max_w: u32,
        max_h: u32,
    ) -> bool {
        match self.inner {
            TimelineInner::Video(v) => v.try_paint_hover_thumb(frame, dest, max_w, max_h),
        }
    }

    pub fn cancel_hover_thumb(self) {
        match self.inner {
            TimelineInner::Video(v) => v.cancel_hover_thumb(),
        }
    }
}

impl From<VideoRef> for TimelineRef {
    fn from(video: VideoRef) -> Self {
        Self::from_video(video)
    }
}

impl VideoRef {
    /// Scrubber / hover surface for this element (shared with future Sequence).
    pub fn as_timeline(self) -> TimelineRef {
        TimelineRef::from_video(self)
    }
}
