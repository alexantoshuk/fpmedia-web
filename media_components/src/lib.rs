//! Leptos media elements over [`webmedia`] — HTML `<video>` / `<audio>` analogs.
//!
//! - [`video`] — frame-accurate `<Video>` + [`VideoRef`]
//! - [`helpers`] — scrubber / cache-strip CSS math, hover-thumb geometry, metadata formatters
//! - [`hover_thumb`] — Frame.io-style scrubber chrome (`HoverThumbPreview`) — not inside `<Video>`
//! - `audio` — planned pure-audio host (`AudioRef`)
//!
//! Host chrome (keyboard, transport buttons) stays in the demo `app` crate.

pub mod helpers;
#[cfg(target_arch = "wasm32")]
pub mod hover_thumb;
#[cfg(target_arch = "wasm32")]
pub mod video;

#[cfg(target_arch = "wasm32")]
pub use hover_thumb::{
    DEFAULT_HOVER_THUMB_MAX_H, DEFAULT_HOVER_THUMB_MAX_W, HoverThumbApi, HoverThumbPreview,
};
#[cfg(target_arch = "wasm32")]
pub use video::{
    PipelineStats, ReadyFrameKind, ReadyFrameMark, ThumbPolicy, Video, VideoMetadata, VideoRef,
};
