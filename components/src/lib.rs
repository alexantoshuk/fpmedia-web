//! Leptos media elements over [`fpmedia_web`] — HTML `<video>` / `<audio>` analogs.
//!
//! - [`video`] — frame-precise `<Video>` + [`VideoRef`]
//! - [`timeline`] — shared [`TimelineRef`] for scrub/hover (Video / VideoPlaylist)
//! - [`scrubber`] — headless `<Scrubber>` (gesture + context)
//! - [`timeline_meta`] — `<TimelineMeta>` (one fetch/residency strip)
//! - [`hover_thumb`] — `<HoverThumb>` (paint inside; host chrome via children)
//! - [`strip`] — strip pixel-run math + 1 px canvas paint
//! - [`helpers`] — playhead / scrub-x CSS math, hover geometry, metadata formatters
//! - [`video_playlist`] — playlist model + `[i]` helpers + [`VideoPlaylist`] / [`VideoPlaylistRef`] (wasm)
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
pub mod video_playlist;

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
pub use video_playlist::{
    PlaylistClip, PlaylistModel, PlaylistPos, format_playlist_frames, format_playlist_goto_frames,
    format_playlist_goto_timecode, format_playlist_timecode, parse_playlist_goto,
    split_playlist_goto,
};
#[cfg(target_arch = "wasm32")]
pub use video_playlist::{VideoPlaylist, VideoPlaylistRef};
