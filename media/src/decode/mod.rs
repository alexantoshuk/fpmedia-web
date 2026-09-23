//! Browser decode / render plumbing (WebCodecs + canvas + scrub blit).
//!
//! Orchestration lives in [`crate::video_player::VideoPlayer`].

pub(crate) mod canvas;
pub(crate) mod frame_cache;
pub(crate) mod owned_video_frame;
pub(crate) mod scrub_blit;
#[cfg(feature = "thumbs")]
pub(crate) mod thumb_preview;
pub(crate) mod webcodecs;

pub use canvas::{
    canvas_2d, configure_canvas_for_video, draw_frame_to_canvas, draw_frame_with_context,
};
pub use owned_video_frame::{AsVideoFrame, OwnedVideoFrame};
#[cfg(feature = "thumbs")]
pub use thumb_preview::{ThumbPreview, ThumbSink};
pub use webcodecs::WebCodecsDecoder;
