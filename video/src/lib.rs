//! Leptos `<Video>` element — frame-accurate media stage over [`webmedia::WebPlayer`].
//!
//! Analogous to HTML `<video>`: owns canvas, open/decode, play clock, seek/step.
//! Host UI (controls, file picker) lives outside this crate.

mod component;
mod op_queue;

pub use component::Video;
pub use webmedia::{PipelineStats, ReadyFrameKind, ReadyFrameMark, VideoMetadata};

use std::rc::Rc;

use leptos::prelude::*;

/// Cloneable handle to a mounted [`Video`] — like a ref to an HTMLMediaElement.
#[derive(Clone, Copy)]
pub struct VideoRef {
    pub busy: RwSignal<bool>,
    pub ready: RwSignal<bool>,
    pub playing: RwSignal<bool>,
    pub looping: RwSignal<bool>,
    /// Engine-committed presentation frame (may lag during async scrub).
    pub frame: RwSignal<u32>,
    /// UI / transport playhead — scrubber, Prev/Next/Seek/Play/Pause target.
    /// Scrub updates this immediately without locking; sync ops promote it and
    /// drive the engine from this value (not from a stale `frame`).
    pub playhead: RwSignal<u32>,
    pub frame_count: RwSignal<u32>,
    pub timestamp: RwSignal<f64>,
    pub fps: RwSignal<Option<f64>>,
    pub metadata: RwSignal<Option<VideoMetadata>>,
    pub error: RwSignal<Option<String>>,
    pub pipeline: RwSignal<Option<PipelineStats>>,
    pub status: RwSignal<String>,
    /// Non-`Send` callbacks registered by the mounted [`Video`] component.
    pub(crate) ops: StoredValue<Option<VideoOps>, LocalStorage>,
}

#[derive(Clone)]
pub(crate) struct VideoOps {
    pub open_blob: Rc<dyn Fn(web_sys::Blob)>,
    pub play: Rc<dyn Fn()>,
    pub pause: Rc<dyn Fn()>,
    pub seek_frame: Rc<dyn Fn(u32)>,
    /// Scrub-drag preview: nearest sync at/before frame (mediabunny `seekToKey`).
    pub seek_keyframe: Rc<dyn Fn(u32)>,
    /// Async timeline scrub — cancelable exact seek (latest wins).
    /// `settle` = pointer released (exact-first); drag keeps densify warming.
    pub scrub_frame: Rc<dyn Fn(u32, bool)>,
    pub next_frame: Rc<dyn Fn()>,
    pub previous_frame: Rc<dyn Fn()>,
    pub set_loop: Rc<dyn Fn(bool)>,
}

impl VideoRef {
    pub fn new() -> Self {
        Self {
            busy: RwSignal::new(false),
            ready: RwSignal::new(false),
            playing: RwSignal::new(false),
            looping: RwSignal::new(false),
            frame: RwSignal::new(0),
            playhead: RwSignal::new(0),
            frame_count: RwSignal::new(0),
            timestamp: RwSignal::new(0.0),
            fps: RwSignal::new(None),
            metadata: RwSignal::new(None),
            error: RwSignal::new(None),
            pipeline: RwSignal::new(None),
            status: RwSignal::new(String::from("Load an MP4 to begin.")),
            ops: StoredValue::new_local(None),
        }
    }

    pub fn open_blob(&self, blob: web_sys::Blob) {
        let open = self
            .ops
            .try_with_value(|ops| ops.as_ref().map(|o| Rc::clone(&o.open_blob)))
            .flatten();
        if let Some(open) = open {
            open(blob);
        } else {
            self.status
                .set("Video not mounted — hard-refresh the page.".into());
            self.error.set(Some("Video not mounted".into()));
        }
    }

    pub fn play(&self) {
        // Start from the UI playhead (may be ahead of engine after scrub).
        let ph = self.playhead.get_untracked();
        if self.frame.get_untracked() != ph {
            self.seek_frame(ph);
        }
        if let Some(play) = self
            .ops
            .try_with_value(|ops| ops.as_ref().map(|o| Rc::clone(&o.play)))
            .flatten()
        {
            play();
        }
    }

    pub fn pause(&self) {
        // Promote potential playhead to the transport target, then pause.
        let ph = self.playhead.get_untracked();
        if self.frame.get_untracked() != ph {
            self.seek_frame(ph);
        }
        if let Some(pause) = self
            .ops
            .try_with_value(|ops| ops.as_ref().map(|o| Rc::clone(&o.pause)))
            .flatten()
        {
            pause();
        }
    }

    pub fn seek_frame(&self, frame: u32) {
        let max = self.frame_count.get_untracked().saturating_sub(1);
        let frame = frame.min(max);
        self.playhead.set(frame);
        if let Some(seek) = self
            .ops
            .try_with_value(|ops| ops.as_ref().map(|o| Rc::clone(&o.seek_frame)))
            .flatten()
        {
            seek(frame);
        }
    }

    /// Fast scrub preview — snap to the nearest keyframe at or before `frame`.
    pub fn seek_keyframe(&self, frame: u32) {
        let max = self.frame_count.get_untracked().saturating_sub(1);
        let frame = frame.min(max);
        self.playhead.set(frame);
        if let Some(seek) = self
            .ops
            .try_with_value(|ops| ops.as_ref().map(|o| Rc::clone(&o.seek_keyframe)))
            .flatten()
        {
            seek(frame);
        }
    }

    /// Async scrub drag — cancelable exact seek; rapid moves keep only the latest.
    /// Updates [`Self::playhead`] immediately; does not latch `busy`.
    /// Densify runs while dragging so the cache warms under the finger.
    pub fn scrub_frame(&self, frame: u32) {
        self.scrub_frame_inner(frame, false);
    }

    /// Pointer-up settle — exact frame first (hold densify), then densify the GOP.
    pub fn scrub_settle(&self, frame: u32) {
        self.scrub_frame_inner(frame, true);
    }

    fn scrub_frame_inner(&self, frame: u32, settle: bool) {
        let max = self.frame_count.get_untracked().saturating_sub(1);
        let frame = frame.min(max);
        self.playhead.set(frame);
        if let Some(scrub) = self
            .ops
            .try_with_value(|ops| ops.as_ref().map(|o| Rc::clone(&o.scrub_frame)))
            .flatten()
        {
            scrub(frame, settle);
        }
    }

    pub fn next_frame(&self) {
        let max = self.frame_count.get_untracked().saturating_sub(1);
        let ph = self.playhead.get_untracked().saturating_add(1).min(max);
        self.playhead.set(ph);
        let engine = self.frame.get_untracked();
        if engine < max && engine + 1 == ph {
            if let Some(next) = self
                .ops
                .try_with_value(|ops| ops.as_ref().map(|o| Rc::clone(&o.next_frame)))
                .flatten()
            {
                next();
            }
        } else if let Some(seek) = self
            .ops
            .try_with_value(|ops| ops.as_ref().map(|o| Rc::clone(&o.seek_frame)))
            .flatten()
        {
            seek(ph);
        }
    }

    pub fn previous_frame(&self) {
        let ph = self.playhead.get_untracked().saturating_sub(1);
        self.playhead.set(ph);
        let engine = self.frame.get_untracked();
        if engine > 0 && engine - 1 == ph {
            if let Some(prev) = self
                .ops
                .try_with_value(|ops| ops.as_ref().map(|o| Rc::clone(&o.previous_frame)))
                .flatten()
            {
                prev();
            }
        } else if let Some(seek) = self
            .ops
            .try_with_value(|ops| ops.as_ref().map(|o| Rc::clone(&o.seek_frame)))
            .flatten()
        {
            seek(ph);
        }
    }

    pub fn set_loop(&self, on: bool) {
        self.looping.set(on);
        if let Some(set_loop) = self
            .ops
            .try_with_value(|ops| ops.as_ref().map(|o| Rc::clone(&o.set_loop)))
            .flatten()
        {
            set_loop(on);
        }
    }
}

impl Default for VideoRef {
    fn default() -> Self {
        Self::new()
    }
}
