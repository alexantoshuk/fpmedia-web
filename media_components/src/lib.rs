//! Leptos media elements over [`webmedia`] — HTML `<video>` / `<audio>` analogs.
//!
//! - [`video`] — frame-accurate `<Video>` + [`VideoRef`]
//! - [`strip`] — scrubber strip pixel runs (fetch / Covered / Exact) + 1 px canvas paint
//! - [`helpers`] — playhead / scrub-x CSS math, hover-thumb geometry, metadata formatters
//! - [`hover_thumb`] — hover paint helper (`HoverThumbPreview`; agreed `<HoverThumb>` in `.ai/product.md`) — not player chrome
//! - `audio` — planned pure-audio host (`AudioRef`)
//!
//! Player chrome (keyboard, transport, scrubber look) stays in the demo `app` crate.

pub mod helpers;
#[cfg(target_arch = "wasm32")]
pub mod hover_thumb;
pub mod strip;
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
