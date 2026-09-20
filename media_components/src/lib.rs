//! Leptos media elements over [`webmedia`] — HTML `<video>` / `<audio>` analogs.
//!
//! - [`video`] — frame-accurate `<Video>` + [`VideoRef`]
//! - [`helpers`] — scrubber / cache-strip CSS math + metadata formatters
//! - `audio` — planned pure-audio host (`AudioRef`)
//!
//! Host chrome (keyboard, transport buttons) stays in the demo `app` crate.

pub mod helpers;
pub mod video;

pub use video::{PipelineStats, ReadyFrameKind, ReadyFrameMark, Video, VideoMetadata, VideoRef};
