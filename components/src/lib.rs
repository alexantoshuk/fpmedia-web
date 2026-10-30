//! Leptos media elements over [`fpmedia_web`] — HTML `<video>` / `<audio>` analogs.
//!
//! - [`video`] — frame-precise `<Video>` + [`VideoRef`]
//! - [`timeline`] — shared [`TimelineRef`] for scrub/hover (Video now; Sequence later)
//! - [`scrubber`] — headless `<Scrubber>` (gesture + context)
//! - [`timeline_meta`] — `<TimelineMeta>` (one fetch/residency strip)
//! - [`hover_thumb`] — `<HoverThumb>` (paint inside; host chrome via children)
//! - [`strip`] — strip pixel-run math + 1 px canvas paint
//! - [`helpers`] — playhead / scrub-x CSS math, hover geometry, metadata formatters
//! - `audio` — planned pure-audio host (`AudioRef`)
//!
//! Player chrome (keyboard, transport, scrubber look) stays in the demo `app` crate.
//! Spec: `.ai/product.md` § Element-layer helpers.

pub mod helpers;
#[cfg(target_arch = "wasm32")]
pub mod hover_thumb;
#[cfg(target_arch = "wasm32")]
pub mod scrubber;
pub mod strip;
#[cfg(target_arch = "wasm32")]
pub mod timeline;
#[cfg(target_arch = "wasm32")]
pub mod timeline_meta;
#[cfg(target_arch = "wasm32")]
pub mod video;

#[cfg(target_arch = "wasm32")]
pub use hover_thumb::{
    DEFAULT_HOVER_THUMB_MAX_H, DEFAULT_HOVER_THUMB_MAX_W, HoverThumb, HoverThumbCtx,
    HoverThumbPreview,
};
#[cfg(target_arch = "wasm32")]
pub use scrubber::{HoverThumbApi, Scrubber, ScrubberCtx};
#[cfg(target_arch = "wasm32")]
pub use timeline::TimelineRef;
#[cfg(target_arch = "wasm32")]
pub use timeline_meta::{TimelineMeta, TimelineMetaKind};
#[cfg(target_arch = "wasm32")]
pub use video::{
    PipelineInterest, PipelineInterestLease, PipelinePlane, PipelineStats, ReadyFrameKind,
    ReadyFrameMark, ScrubCachePolicy, ThumbPolicy, Video, VideoMetadata, VideoRef,
};
