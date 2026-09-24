//! Frame-accurate media engine for webmedia.
//!
//! Isolated from the Leptos/`app` stack.
//!
//! - Always: MP4 demux + presentation-order frame index + player API (encoded samples).
//! - Feature `webcodecs` on `wasm32`: WebCodecs decode → canvas via [`VideoPlayer`].
//!
//! Frame index (presentation order) is the primary position identifier —
//! timestamps are derived from the sample table, not `time * fps`.

mod audio;
mod bitstream;
mod clock;
mod demux;
mod error;
mod frame_index;
mod host_op_coalesce;
mod playback_policy;
mod transport;
mod player;
mod source;
mod types;
mod util;

#[cfg(all(target_arch = "wasm32", feature = "webcodecs"))]
mod decode;
#[cfg(all(target_arch = "wasm32", feature = "webcodecs"))]
mod video_player;

#[cfg(all(target_arch = "wasm32", feature = "webcodecs"))]
pub use audio::{AudioOutput, OwnedAudioData};
pub use audio::{AudioSampleEntry, AudioSampleId, Mp4AudioTrack, TimeStretcher};

pub use bitstream::{
    PacketType, accept_avc_recovery_point_sei, avc_nal_length_size, determine_avc_packet_type,
    determine_avc_packet_type_with, determine_hevc_packet_type, has_hevc_rasl_picture,
    hevc_nal_length_size, sanitize_avc_packet_for_chromium, sanitize_hevc_packet_for_chromium,
};
pub use clock::{
    MediaClock, PLAYBACK_RATE_AUDIO_MAX, PLAYBACK_RATE_MAX, PLAYBACK_RATE_MIN,
    PLAYBACK_RATE_REVIEW_MAX, PLAYBACK_RATE_SHUTTLE_MIN, PLAYBACK_RATE_STEP, is_shuttle_rate,
    is_wall_clock_rate, normalize_playback_rate,
};
#[cfg(not(target_arch = "wasm32"))]
pub use demux::open_mp4_path;
pub use demux::{
    Mp4VideoTrack, OpenMp4Options, OpenedMp4, TopLevelBox, load_ftyp_moov, open_mp4_bytes,
    open_mp4_media_bytes, open_mp4_media_source_cached, open_mp4_media_source_cached_with_progress,
    open_mp4_source, open_mp4_source_cached, open_mp4_source_cached_with_progress,
    open_mp4_source_with_progress, parse_mp4_progressive, parse_mp4_progressive_with_progress,
    read_ftyp_moov_stub, scan_top_level,
};
pub use error::MediaError;
pub use frame_index::{FrameEntry, FrameIndex};
pub use host_op_coalesce::{
    HostOpKind, apply_play, apply_scrub, apply_scrub_release, apply_seek_or_step,
};
pub use transport::{
    TransportCmd, TransportEvent, TransportModel, TransportState, reduce as transport_reduce,
};
pub use playback_policy::{
    EofWall, eof_timeline_end, eof_wall_decision, loop_drain_frames, near_eof_by_frames,
};
pub use player::{DecodeWindow, Player, PlayerState};
#[cfg(all(target_arch = "wasm32", feature = "webcodecs"))]
pub use source::{BlobSource, UrlSource, UrlSourceOptions, is_no_http_range};
pub use source::{
    CachedSource, DEFAULT_CACHE_BUDGET, DEFAULT_CACHE_SIZE, DEFAULT_URL_PARALLELISM, FetchMap,
    MediaSource, MemorySource, OPEN_URL_PARALLELISM, ReadFuture,
};
pub use types::{
    AudioCodec, AudioCodecConfig, AudioMetadata, CodecConfig, DEFAULT_SCRUB_BLIT_MIB_LONG,
    DEFAULT_SCRUB_BLIT_MIB_SHORT, DEFAULT_SCRUB_CACHE_MIB, EDGE_MARGIN_LEFT_MAX,
    EDGE_MARGIN_LEFT_MIN, EDGE_MARGIN_RIGHT_MIN, EncodedAudioSample, EncodedSample, FrameId,
    GopProfile, MAX_SCRUB_BLIT_FRAMES, MAX_SCRUB_CACHE_FRAMES, MIN_SCRUB_BLIT_FRAMES,
    MIN_SCRUB_CACHE_FRAMES, PLAY_EOF_BUDGET_SECS_HD, PLAY_EOF_BUDGET_SECS_UHD,
    PLAY_EOF_LEAD_SECS_HD, PLAY_EOF_LEAD_SECS_UHD, PLAY_LOOP_PRIME_FRAMES_HD,
    PLAY_LOOP_PRIME_FRAMES_UHD, PLAY_PRESENTATION_CAP_HD, PLAY_PRESENTATION_CAP_UHD,
    PLAY_WARM_AHEAD_HD, PLAY_WARM_AHEAD_UHD, PlayPolicy, ResolutionClass, SCRUB_BLIT_LRU,
    SHORT_GOP_MAX_LEN, ScrubDragMode, ScrubPolicy, ThumbPolicy, Timestamp, UHD_MIN_HEIGHT,
    VideoCodec, VideoMetadata, build_aac_asc, coalesce_frame_runs, estimated_decoded_frame_bytes,
    exact_frames_to_free_for_play_vram, hover_thumb_dims, nearer_gap_with_dir,
    next_missing_sync_key, scrub_blit_cap_during_play, scrub_blit_frame_budget,
    scrub_cache_frame_budget, scrub_edge_margins, scrub_gap_fill_armed, scrub_preview_choice,
};

#[cfg(all(target_arch = "wasm32", feature = "webcodecs"))]
pub use decode::{
    AsVideoFrame, OwnedVideoFrame, WebCodecsDecoder, canvas_2d, configure_canvas_for_video,
    draw_frame_to_canvas, draw_frame_with_context,
};
#[cfg(all(target_arch = "wasm32", feature = "webcodecs", feature = "thumbs"))]
pub use decode::{ThumbPreview, ThumbSink};
#[cfg(all(target_arch = "wasm32", feature = "webcodecs"))]
pub use video_player::{
    DuePresent, PRESENTATION_QUEUE_CAP, PipelineStats, ReadyFrameKind, ReadyFrameMark,
    STEP_BEHIND_CAP, VideoPlayer,
};
