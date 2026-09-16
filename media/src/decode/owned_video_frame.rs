//! RAII owner for [`web_sys::VideoFrame`] — `close()` on drop.
//!
//! # Ownership rules
//!
//! - **Prefer moving** `OwnedVideoFrame` into a cache / queue (unique ownership).
//! - **Draw via borrow** ([`AsVideoFrame`] / [`OwnedVideoFrame::as_web_sys`]) — no clone.
//! - **[`OwnedVideoFrame::try_clone`] is fallible and often fails** in browsers. It returns
//!   [`Result`], never `Option`, so a failed clone cannot be mistaken for “skip insert”.
//!
//! Escape hatch: [`OwnedVideoFrame::into_raw`] when a bare `VideoFrame` is required
//! (caller then owns `close`).

use std::mem::ManuallyDrop;
use std::ops::Deref;

use web_sys::VideoFrame;

use crate::error::MediaError;

/// Borrow the WebCodecs handle for draw / inspect without cloning.
///
/// Use this (or [`OwnedVideoFrame::as_web_sys`]) when presenting a cached frame.
/// Do **not** clone just to call `drawImage`.
pub trait AsVideoFrame {
    fn as_video_frame(&self) -> &VideoFrame;
}

/// Owned WebCodecs frame handle; closes the browser resource on drop.
pub struct OwnedVideoFrame(VideoFrame);

impl OwnedVideoFrame {
    pub fn new(frame: VideoFrame) -> Self {
        Self(frame)
    }

    pub fn as_web_sys(&self) -> &VideoFrame {
        &self.0
    }

    /// Fallible browser `VideoFrame.clone`.
    ///
    /// **Do not use this to populate a cache.** Move `self` into scrub ingest after draw
    /// (see `WebPlayer::present_and_cache` / WebP `put_chunk`). Clone is
    /// only for handing an owned copy to an API that requires `OwnedVideoFrame`
    /// (e.g. `frame_at`).
    ///
    /// Returns [`Err`] on failure — never `None` — so callers cannot silently drop
    /// the failure with `if let Some(clone) = … { insert(clone) }`.
    #[must_use = "clone failure must be handled; do not discard with `.ok()` to skip caching"]
    pub fn try_clone(&self) -> Result<Self, MediaError> {
        self.0.clone().map(Self).map_err(|e| {
            MediaError::VideoFrameClone(format!(
                "VideoFrame.clone failed ({e:?}); move the frame instead of cloning for cache"
            ))
        })
    }

    /// Move out the raw handle without closing. Caller must `.close()` (or wrap again).
    pub fn into_raw(self) -> VideoFrame {
        let this = ManuallyDrop::new(self);
        // Safety: `this` is not dropped; we take ownership of the inner frame.
        unsafe { std::ptr::read(&this.0) }
    }
}

impl AsVideoFrame for OwnedVideoFrame {
    fn as_video_frame(&self) -> &VideoFrame {
        &self.0
    }
}

impl Deref for OwnedVideoFrame {
    type Target = VideoFrame;

    fn deref(&self) -> &VideoFrame {
        &self.0
    }
}

impl Drop for OwnedVideoFrame {
    fn drop(&mut self) {
        self.0.close();
    }
}

impl From<VideoFrame> for OwnedVideoFrame {
    fn from(frame: VideoFrame) -> Self {
        Self::new(frame)
    }
}
