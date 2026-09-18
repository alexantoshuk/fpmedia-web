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
mod clock;
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
    PacketType, accept_avc_recovery_point_sei, avc_nal_length_size, determine_avc_packet_type,
    determine_avc_packet_type_with, sanitize_avc_packet_for_chromium,
};
pub use clock::MediaClock;
#[cfg(not(target_arch = "wasm32"))]
pub use demux::open_mp4_path;
pub use demux::{
    AudioSampleEntry, AudioSampleId, Mp4AudioTrack, Mp4VideoTrack, OpenMp4Options, OpenedMp4,
    TopLevelBox, load_ftyp_moov, open_mp4_bytes, open_mp4_media_bytes,
    open_mp4_media_source_cached, open_mp4_media_source_cached_with_progress, open_mp4_source,
    open_mp4_source_cached, open_mp4_source_cached_with_progress, open_mp4_source_with_progress,
    parse_mp4_progressive, parse_mp4_progressive_with_progress, read_ftyp_moov_stub,
    scan_top_level,
};
pub use error::MediaError;
pub use frame_index::{FrameEntry, FrameIndex};
pub use player::{DecodeWindow, Player, PlayerState};
#[cfg(all(target_arch = "wasm32", feature = "webcodecs"))]
pub use source::BlobSource;
pub use source::{CachedSource, DEFAULT_CACHE_SIZE, MediaSource, MemorySource, ReadFuture};
pub use types::{
    AudioCodec, AudioCodecConfig, AudioMetadata, CodecConfig, DEFAULT_SCRUB_CACHE_MIB,
    EncodedAudioSample, EncodedSample, FrameId, GopProfile, MAX_SCRUB_CACHE_FRAMES,
    MIN_SCRUB_CACHE_FRAMES, SCRUB_BLIT_LRU, SCRUB_CACHE_EDGE_MARGIN, SHORT_GOP_MAX_LEN,
    ScrubDragMode, ScrubPolicy, Timestamp, VideoCodec, VideoMetadata, build_aac_asc,
    coalesce_frame_runs, estimated_decoded_frame_bytes, nearer_gap_with_dir,
    scrub_cache_frame_budget, scrub_preview_choice,
};

#[cfg(all(target_arch = "wasm32", feature = "webcodecs"))]
pub use decode::{
    AsVideoFrame, AudioOutput, DuePresent, OwnedAudioData, OwnedVideoFrame, PRESENTATION_QUEUE_CAP,
    PipelineStats, ReadyFrameKind, ReadyFrameMark, STEP_BEHIND_CAP, WebCodecsDecoder, WebPlayer,
    canvas_2d, configure_canvas_for_video, draw_frame_to_canvas, draw_frame_with_context,
};
