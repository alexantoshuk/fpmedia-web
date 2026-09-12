//! Frame-accurate media engine for webmedia.
//!
//! Isolated from the Leptos/`app` stack.
//!
//! - Always: MP4 demux + presentation-order frame index + player API (encoded samples).
//! - Feature `webcodecs` on `wasm32`: WebCodecs decode → canvas via [`WebPlayer`].
//!
//! Frame index (presentation order) is the primary position identifier —
//! timestamps are derived from the sample table, not `time * fps`.

mod bitstream;
mod demux;
mod error;
mod frame_index;
mod player;
mod source;
mod types;
mod util;

#[cfg(all(target_arch = "wasm32", feature = "webcodecs"))]
mod decode;

pub use bitstream::{
    PacketType, avc_nal_length_size, determine_avc_packet_type, sanitize_avc_packet_for_chromium,
};
pub use demux::{
    Mp4VideoTrack, OpenMp4Options, TopLevelBox, load_ftyp_moov, open_mp4_bytes, open_mp4_source,
    open_mp4_source_cached, open_mp4_source_cached_with_progress, open_mp4_source_with_progress,
    parse_mp4_progressive, parse_mp4_progressive_with_progress, read_ftyp_moov_stub,
    scan_top_level,
};
#[cfg(not(target_arch = "wasm32"))]
pub use demux::open_mp4_path;
pub use error::MediaError;
pub use frame_index::{FrameEntry, FrameIndex};
pub use player::{DecodeWindow, Player, PlayerState};
pub use source::{CachedSource, DEFAULT_CACHE_SIZE, MediaSource, MemorySource, ReadFuture};
#[cfg(all(target_arch = "wasm32", feature = "webcodecs"))]
pub use source::BlobSource;
pub use types::{
    CodecConfig, EncodedSample, FrameId, Timestamp, VideoCodec, VideoMetadata,
};

#[cfg(all(target_arch = "wasm32", feature = "webcodecs"))]
pub use decode::{
    DuePresent, AsVideoFrame, OwnedVideoFrame, PipelineStats, PRESENTATION_QUEUE_CAP, ReadyFrameKind,
    ReadyFrameMark, WebCodecsDecoder, WebPlayer, canvas_2d, configure_canvas_for_video,
    draw_frame_to_canvas, draw_frame_with_context,
};
