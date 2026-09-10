//! Leptos `<Video>` element — frame-accurate media stage over [`webmedia::WebPlayer`].
//!
//! Analogous to HTML `<video>`: owns canvas, open/decode, play clock, seek/step.
//! Host UI (controls, file picker) lives outside this crate.

mod component;

pub use component::Video;
pub use webmedia::{PipelineStats, VideoMetadata};

use std::rc::Rc;

use leptos::prelude::*;

/// Cloneable handle to a mounted [`Video`] — like a ref to an HTMLMediaElement.
#[derive(Clone, Copy)]
pub struct VideoRef {
    pub busy: RwSignal<bool>,
    pub ready: RwSignal<bool>,
    pub playing: RwSignal<bool>,
    pub looping: RwSignal<bool>,
    pub frame: RwSignal<u32>,
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
    pub open: Rc<dyn Fn(Vec<u8>)>,
    pub play: Rc<dyn Fn()>,
    pub pause: Rc<dyn Fn()>,
    pub seek_frame: Rc<dyn Fn(u32)>,
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

    pub fn open_mp4_bytes(&self, bytes: Vec<u8>) {
        let open = self
            .ops
            .try_with_value(|ops| ops.as_ref().map(|o| Rc::clone(&o.open)))
            .flatten();
        if let Some(open) = open {
            open(bytes);
        } else {
            self.error.set(Some("Video not mounted".into()));
        }
    }

    pub fn play(&self) {
        if let Some(play) = self
            .ops
            .try_with_value(|ops| ops.as_ref().map(|o| Rc::clone(&o.play)))
            .flatten()
        {
            play();
        }
    }

    pub fn pause(&self) {
        if let Some(pause) = self
            .ops
            .try_with_value(|ops| ops.as_ref().map(|o| Rc::clone(&o.pause)))
            .flatten()
        {
            pause();
        }
    }

    pub fn seek_frame(&self, frame: u32) {
        if let Some(seek) = self
            .ops
            .try_with_value(|ops| ops.as_ref().map(|o| Rc::clone(&o.seek_frame)))
            .flatten()
        {
            seek(frame);
        }
    }

    pub fn next_frame(&self) {
        if let Some(next) = self
            .ops
            .try_with_value(|ops| ops.as_ref().map(|o| Rc::clone(&o.next_frame)))
            .flatten()
        {
            next();
        }
    }

    pub fn previous_frame(&self) {
        if let Some(prev) = self
            .ops
            .try_with_value(|ops| ops.as_ref().map(|o| Rc::clone(&o.previous_frame)))
            .flatten()
        {
            prev();
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
