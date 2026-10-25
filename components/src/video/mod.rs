//! Leptos `<Video>` element — frame-precise media stage over [`fpmedia_web::VideoPlayer`].
//!
//! Analogous to HTML `<video>`: owns canvas, open/decode, play clock, seek/step.
//! Host UI (controls, file picker) lives in the demo `app` crate.

mod component;
mod ctx;
mod host_ops;
mod op_queue;
mod open_url;
mod pipeline_interest;
mod session;
mod transport_apply;

pub use component::Video;
pub use fpmedia_web::{
    PipelineInterest, PipelineStats, ReadyFrameKind, ReadyFrameMark, ScrubCachePolicy, ThumbPolicy,
    VideoMetadata,
};
pub use pipeline_interest::{PipelineInterestLease, PipelinePlane};

use std::rc::Rc;

use leptos::prelude::*;
use web_sys::HtmlCanvasElement;

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
    /// Frame.io-style playback rate (forward 0.25–8).
    pub playback_rate: RwSignal<f64>,
    /// Non-`Send` callbacks registered by the mounted [`Video`] component.
    pub(crate) ops: StoredValue<Option<VideoOps>, LocalStorage>,
    /// Refcounts for [`PipelineInterest`] planes (strips / debug meters).
    pipeline_interest_counts: StoredValue<pipeline_interest::InterestCounts, LocalStorage>,
}

#[derive(Clone)]
pub(crate) struct VideoOps {
    pub open_blob: Rc<dyn Fn(web_sys::Blob)>,
    /// Host-layer URL open. Accepts `https:` / `http:` (HTTP Range via
    /// [`fpmedia_web::UrlSource`] when the server supports it; full-download
    /// fallback otherwise) and same-document `blob:`.
    pub open_url: Rc<dyn Fn(String)>,
    pub play: Rc<dyn Fn()>,
    pub pause: Rc<dyn Fn()>,
    /// Tear down the decode session and free caches (keeps the mounted canvas).
    pub close: Rc<dyn Fn()>,
    pub seek_frame: Rc<dyn Fn(u32)>,
    /// Scrub-drag preview: nearest sync at/before frame (mediabunny `seekToKey`).
    pub seek_keyframe: Rc<dyn Fn(u32)>,
    pub scrub_pointer_down: Rc<dyn Fn(u32)>,
    pub scrub_pointer_move: Rc<dyn Fn(u32)>,
    pub scrub_pointer_up: Rc<dyn Fn(u32)>,
    pub next_frame: Rc<dyn Fn()>,
    pub previous_frame: Rc<dyn Fn()>,
    pub set_loop: Rc<dyn Fn(bool)>,
    pub set_volume: Rc<dyn Fn(f64)>,
    pub set_muted: Rc<dyn Fn(bool)>,
    pub set_playback_rate: Rc<dyn Fn(f64)>,
    /// Hover timeline thumb → paint into `dest` (ThumbPreview / Exact read-only).
    pub paint_hover_thumb: Rc<dyn Fn(u32, HtmlCanvasElement, u32, u32)>,
    pub try_paint_hover_thumb: Rc<dyn Fn(u32, HtmlCanvasElement, u32, u32) -> bool>,
    pub cancel_hover_thumb: Rc<dyn Fn()>,
    pub set_thumb_policy: Rc<dyn Fn(ThumbPolicy)>,
    pub set_scrub_cache_policy: Rc<dyn Fn(ScrubCachePolicy)>,
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
            playback_rate: RwSignal::new(1.0),
            ops: StoredValue::new_local(None),
            pipeline_interest_counts: StoredValue::new_local(
                pipeline_interest::InterestCounts::default(),
            ),
        }
    }

    /// Clone one registered callback out of [`Self::ops`] (call it after the
    /// store borrow ends — callbacks may re-enter `VideoRef`).
    fn op<F: ?Sized>(&self, pick: impl FnOnce(&VideoOps) -> &Rc<F>) -> Option<Rc<F>> {
        self.ops
            .try_with_value(|ops| ops.as_ref().map(|o| Rc::clone(pick(o))))
            .flatten()
    }

    /// Clamp `frame` to the clip, clear `ended`, move the UI playhead there.
    fn aim(&self, frame: u32) -> u32 {
        let max = self.frame_count.get_untracked().saturating_sub(1);
        let frame = frame.min(max);
        self.ended.set(false);
        self.playhead.set(frame);
        frame
    }

    pub fn open_blob(&self, blob: web_sys::Blob) {
        if let Some(open) = self.op(|o| &o.open_blob) {
            open(blob);
        } else {
            self.status
                .set("Video not mounted — hard-refresh the page.".into());
            self.error.set(Some("Video not mounted".into()));
        }
    }

    /// Open a remote or `blob:` URL.
    ///
    /// `https:` / `http:` use progressive [`fpmedia_web::UrlSource`] (HTTP Range)
    /// when the server returns `206` + `Content-Range`; otherwise falls back to
    /// a full download → [`Self::open_blob`]. Same-document `blob:` always goes
    /// through fetch → Blob.
    ///
    /// Cross-origin URLs need a CORS-allowing host (or a same-origin proxy URL
    /// the host supplies). This crate does **not** rewrite URLs.
    pub fn open_url(&self, url: impl Into<String>) {
        let url = url.into();
        if let Some(open) = self.op(|o| &o.open_url) {
            open(url);
        } else {
            self.status
                .set("Video not mounted — hard-refresh the page.".into());
            self.error.set(Some("Video not mounted".into()));
        }
    }

    /// Drop the decode session (decoder + caches). Canvas stays mounted for a later open.
    pub fn close(&self) {
        if let Some(close) = self.op(|o| &o.close) {
            close();
        }
    }

    pub fn play(&self) {
        self.ended.set(false);
        // Do **not** `seek_frame` here when `frame != playhead`. After scrub, settle
        // is already cascading key→playhead; a Play-time seek cancels that work and
        // restarts the GOP (multi-second lag on long-GOP clips). PlayPrime
        // waits behind settle (or seeks only if still mismatched after).
        if let Some(play) = self.op(|o| &o.play) {
            play();
        }
    }

    pub fn pause(&self) {
        // Play→Pause: stop first, then snap playhead to committed engine/canvas.
        // During play, UI playhead lags ~100ms behind (`sync_ui` throttle); seeking
        // to that stale playhead jumped the picture on Pause→Seek / Prev→Next.
        // Already paused with playhead ≠ frame (scrub target): promote via seek.
        let was_playing = self.playing.get_untracked();
        let ph = self.playhead.get_untracked();
        if let Some(pause) = self.op(|o| &o.pause) {
            pause();
        }
        if !was_playing && self.frame.get_untracked() != ph {
            self.seek_frame(ph);
        }
    }

    pub fn seek_frame(&self, frame: u32) {
        let frame = self.aim(frame);
        if let Some(seek) = self.op(|o| &o.seek_frame) {
            seek(frame);
        }
    }

    /// Fast scrub preview — snap to the nearest keyframe at or before `frame`.
    pub fn seek_keyframe(&self, frame: u32) {
        let frame = self.aim(frame);
        if let Some(seek) = self.op(|o| &o.seek_keyframe) {
            seek(frame);
        }
    }

    /// Timeline pointer down — transport FSM (`architecture.md` § State machines).
    pub fn scrub_pointer_down(&self, frame: u32) {
        let frame = self.aim(frame);
        if let Some(f) = self.op(|o| &o.scrub_pointer_down) {
            f(frame);
        }
    }

    /// Timeline pointer move while scrubbing.
    pub fn scrub_pointer_move(&self, frame: u32) {
        let frame = self.aim(frame);
        if let Some(f) = self.op(|o| &o.scrub_pointer_move) {
            f(frame);
        }
    }

    /// Timeline pointer up — settle (± resume play when scrub began while playing).
    pub fn scrub_pointer_up(&self, frame: u32) {
        let frame = self.aim(frame);
        if let Some(f) = self.op(|o| &o.scrub_pointer_up) {
            f(frame);
        }
    }

    /// Advance one presentation frame from the engine, or seek to a divergent
    /// scrub `playhead`. Key-repeat must **not** keep bumping `playhead` ahead of
    /// `frame` — that promoted Step→Seek and jumped several frames on hold.
    pub fn next_frame(&self) {
        let max = self.frame_count.get_untracked().saturating_sub(1);
        let engine = self.frame.get_untracked();
        let ph = self.playhead.get_untracked();
        self.ended.set(false);

        // Scrub/seek target still ahead/behind engine — promote via seek.
        let adjacent_aim = engine.saturating_add(1).min(max);
        if ph != engine && ph != adjacent_aim {
            if let Some(seek) = self.op(|o| &o.seek_frame) {
                seek(ph);
            }
            return;
        }
        if engine >= max {
            return;
        }
        self.playhead.set(adjacent_aim);
        if let Some(next) = self.op(|o| &o.next_frame) {
            next();
        }
    }

    /// Step one frame backward from the engine (same hold-safe rules as
    /// [`Self::next_frame`]).
    pub fn previous_frame(&self) {
        let engine = self.frame.get_untracked();
        let ph = self.playhead.get_untracked();
        self.ended.set(false);

        let adjacent_aim = engine.saturating_sub(1);
        if ph != engine && ph != adjacent_aim {
            if let Some(seek) = self.op(|o| &o.seek_frame) {
                seek(ph);
            }
            return;
        }
        if engine == 0 {
            return;
        }
        self.playhead.set(adjacent_aim);
        if let Some(prev) = self.op(|o| &o.previous_frame) {
            prev();
        }
    }

    pub fn set_loop(&self, on: bool) {
        self.looping.set(on);
        if let Some(set_loop) = self.op(|o| &o.set_loop) {
            set_loop(on);
        }
    }

    pub fn set_volume(&self, volume: f64) {
        let v = volume.clamp(0.0, 1.0);
        self.volume.set(v);
        if let Some(set_volume) = self.op(|o| &o.set_volume) {
            set_volume(v);
        }
    }

    pub fn set_muted(&self, muted: bool) {
        self.muted.set(muted);
        if let Some(set_muted) = self.op(|o| &o.set_muted) {
            set_muted(muted);
        }
    }

    pub fn set_playback_rate(&self, rate: f64) {
        let r = fpmedia_web::normalize_playback_rate(rate);
        self.playback_rate.set(r);
        if let Some(set_rate) = self.op(|o| &o.set_playback_rate) {
            set_rate(r);
        }
    }

    /// Paint a Frame.io-style hover thumb into `dest`.
    ///
    /// Read-only: ThumbPreview cache / Exact downscale — does not decode or move
    /// playhead/canvas. Prefer [`crate::HoverThumbPreview`]; call
    /// [`Self::cancel_hover_thumb`] on leave/scrub. Prefer
    /// [`Self::try_paint_hover_thumb`] on pointermove for cache hits.
    pub fn paint_hover_thumb(&self, frame: u32, dest: HtmlCanvasElement, max_w: u32, max_h: u32) {
        let max = self.frame_count.get_untracked().saturating_sub(1);
        let frame = frame.min(max);
        if let Some(paint) = self.op(|o| &o.paint_hover_thumb) {
            paint(frame, dest, max_w, max_h);
        }
    }

    /// Sync paint from thumb / Exact blit cache only. `true` if `dest` was painted.
    pub fn try_paint_hover_thumb(
        &self,
        frame: u32,
        dest: HtmlCanvasElement,
        max_w: u32,
        max_h: u32,
    ) -> bool {
        let max = self.frame_count.get_untracked().saturating_sub(1);
        let frame = frame.min(max);
        self.op(|o| &o.try_paint_hover_thumb)
            .is_some_and(|paint| paint(frame, dest, max_w, max_h))
    }

    /// Abort in-flight hover thumb Exact downscale / hide race with scrub.
    pub fn cancel_hover_thumb(&self) {
        if let Some(cancel) = self.op(|o| &o.cancel_hover_thumb) {
            cancel();
        }
    }

    /// Runtime thumb sink gate (`Off` drops [`fpmedia_web::ThumbPreview`]).
    pub fn set_thumb_policy(&self, policy: ThumbPolicy) {
        if let Some(set) = self.op(|o| &o.set_thumb_policy) {
            set(policy);
        }
    }

    /// Runtime scrub-cache gate — persists across opens. `Off` (no scrub UI)
    /// closes the scrub decoder and drops Covered / Exact / gray fill.
    pub fn set_scrub_cache_policy(&self, policy: ScrubCachePolicy) {
        if let Some(set) = self.op(|o| &o.set_scrub_cache_policy) {
            set(policy);
        }
    }
}

impl Default for VideoRef {
    fn default() -> Self {
        Self::new()
    }
}
