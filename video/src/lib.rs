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
    /// True after playback stops because the last frame was reached (`loop` off).
    /// Cleared on [`Self::open_blob`], [`Self::close`], seek/scrub/step, and play.
    pub ended: RwSignal<bool>,
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
    /// True when the open file has a decodable audio track.
    pub has_audio: RwSignal<bool>,
    pub volume: RwSignal<f64>,
    pub muted: RwSignal<bool>,
    /// Non-`Send` callbacks registered by the mounted [`Video`] component.
    pub(crate) ops: StoredValue<Option<VideoOps>, LocalStorage>,
}

#[derive(Clone)]
pub(crate) struct VideoOps {
    pub open_blob: Rc<dyn Fn(web_sys::Blob)>,
    /// Host-layer fetch → [`Blob`] → same open path. Accepts `https:` and `blob:`.
    pub open_url: Rc<dyn Fn(String)>,
    pub play: Rc<dyn Fn()>,
    pub pause: Rc<dyn Fn()>,
    /// Tear down the decode session and free caches (keeps the mounted canvas).
    pub close: Rc<dyn Fn()>,
    pub seek_frame: Rc<dyn Fn(u32)>,
    /// Scrub-drag preview: nearest sync at/before frame (mediabunny `seekToKey`).
    pub seek_keyframe: Rc<dyn Fn(u32)>,
    /// Async timeline scrub — cancelable exact seek (latest wins).
    /// `settle` = pointer released (exact-first); drag keeps densify warming.
    pub scrub_frame: Rc<dyn Fn(u32, bool)>,
    pub next_frame: Rc<dyn Fn()>,
    pub previous_frame: Rc<dyn Fn()>,
    pub set_loop: Rc<dyn Fn(bool)>,
    pub set_volume: Rc<dyn Fn(f64)>,
    pub set_muted: Rc<dyn Fn(bool)>,
}

impl VideoRef {
    pub fn new() -> Self {
        Self {
            busy: RwSignal::new(false),
            ready: RwSignal::new(false),
            playing: RwSignal::new(false),
            looping: RwSignal::new(false),
            ended: RwSignal::new(false),
            frame: RwSignal::new(0),
            playhead: RwSignal::new(0),
            frame_count: RwSignal::new(0),
            timestamp: RwSignal::new(0.0),
            fps: RwSignal::new(None),
            metadata: RwSignal::new(None),
            error: RwSignal::new(None),
            pipeline: RwSignal::new(None),
            status: RwSignal::new(String::from("Load an MP4 to begin.")),
            has_audio: RwSignal::new(false),
            volume: RwSignal::new(0.7),
            muted: RwSignal::new(false),
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

    /// Fetch `url` into a [`web_sys::Blob`], then open like [`Self::open_blob`].
    ///
    /// Works for `https:` (full download; needs CORS + a progressive MP4) and
    /// same-document `blob:` URLs (`URL.createObjectURL`). Not HTTP Range /
    /// streaming — that would be a future `UrlSource`.
    pub fn open_url(&self, url: impl Into<String>) {
        let url = url.into();
        let open = self
            .ops
            .try_with_value(|ops| ops.as_ref().map(|o| Rc::clone(&o.open_url)))
            .flatten();
        if let Some(open) = open {
            open(url);
        } else {
            self.status
                .set("Video not mounted — hard-refresh the page.".into());
            self.error.set(Some("Video not mounted".into()));
        }
    }

    /// Drop the decode session (decoder + caches). Canvas stays mounted for a later open.
    pub fn close(&self) {
        if let Some(close) = self
            .ops
            .try_with_value(|ops| ops.as_ref().map(|o| Rc::clone(&o.close)))
            .flatten()
        {
            close();
        }
    }

    pub fn play(&self) {
        self.ended.set(false);
        // Do **not** `seek_frame` here when `frame != playhead`. After scrub, settle
        // is already cascading key→playhead; a Play-time seek cancels that work and
        // restarts the GOP (multi-second lag on Wing It–class clips). PlayPrime
        // waits behind settle (or seeks only if still mismatched after).
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
        self.ended.set(false);
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
        self.ended.set(false);
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
        self.ended.set(false);
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
        self.ended.set(false);
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
        self.ended.set(false);
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

    pub fn set_volume(&self, volume: f64) {
        let v = volume.clamp(0.0, 1.0);
        self.volume.set(v);
        if let Some(set_volume) = self
            .ops
            .try_with_value(|ops| ops.as_ref().map(|o| Rc::clone(&o.set_volume)))
            .flatten()
        {
            set_volume(v);
        }
    }

    pub fn set_muted(&self, muted: bool) {
        self.muted.set(muted);
        if let Some(set_muted) = self
            .ops
            .try_with_value(|ops| ops.as_ref().map(|o| Rc::clone(&o.set_muted)))
            .flatten()
        {
            set_muted(muted);
        }
    }
}

impl Default for VideoRef {
    fn default() -> Self {
        Self::new()
    }
}
