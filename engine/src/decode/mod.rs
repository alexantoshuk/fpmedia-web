//! Browser decode / render plumbing (WebCodecs + canvas + scrub blit).
//!
//! Orchestration lives in [`crate::video_player::VideoPlayer`].
//! Thumb generation product: [`thumb_sink::ThumbSink`] (feature `thumbs`).

pub(crate) mod canvas;
pub(crate) mod frame_cache;
pub(crate) mod owned_video_frame;
pub(crate) mod scrub_blit;
#[cfg(feature = "thumbs")]
pub(crate) mod thumb_sink;
pub(crate) mod webcodecs;

pub use canvas::{
    PLACEHOLDER_CANVAS_H, PLACEHOLDER_CANVAS_W, canvas_2d, configure_canvas_for_video,
    draw_frame_to_canvas, draw_frame_with_context, reset_canvas_layout,
};
pub use owned_video_frame::{AsVideoFrame, OwnedVideoFrame};
#[cfg(feature = "thumbs")]
pub use thumb_sink::{
    DEFAULT_THUMB_MAX_H, DEFAULT_THUMB_MAX_W, ThumbSink, ThumbSinkOptions, paint_thumb_canvas,
};
pub use webcodecs::WebCodecsDecoder;
