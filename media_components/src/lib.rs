//! Leptos media elements over [`webmedia`] — HTML `<video>` / `<audio>` analogs.
//!
//! - [`video`] — frame-accurate `<Video>` + [`VideoRef`]
//! - `audio` — planned pure-audio host (`AudioRef`)
//! - `helpers` — planned shared UI math (cache strips, time formatters)
//!
//! Host chrome (keyboard, transport buttons) stays in the demo `app` crate.

pub mod video;

pub use video::{PipelineStats, ReadyFrameKind, ReadyFrameMark, Video, VideoMetadata, VideoRef};
