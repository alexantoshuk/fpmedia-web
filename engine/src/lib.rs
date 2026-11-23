//! Frame-precise media engine for fpmedia-web.
//!
//! Isolated from the Leptos/`app` stack.
//!
//! - Always: MP4 demux + presentation-order frame index + decode cascades (encoded samples).
//! - Feature `webcodecs` on `wasm32`: WebCodecs decode → canvas via [`VideoPlayer`].
//!
//! Frame index (presentation order) is the primary position identifier —
//! timestamps are derived from the sample table, not `time * fps`.

mod audio;
pub mod bitstream;
mod clock;
mod demux;
mod epoch;
mod error;
mod frame_index;
mod host;
mod model;
mod policy;
mod residency;
mod source;
mod types;
mod util;

#[cfg(all(target_arch = "wasm32", feature = "webcodecs"))]
mod decode;
#[cfg(all(target_arch = "wasm32", feature = "webcodecs"))]
mod video_player;

#[cfg(all(target_arch = "wasm32", feature = "webcodecs"))]
pub mod timer;
#[cfg(all(target_arch = "wasm32", feature = "webcodecs"))]
mod waiters;

#[cfg(all(target_arch = "wasm32", feature = "webcodecs"))]
pub use audio::{AudioOutput, OwnedAudioData};
pub use audio::{AudioSampleEntry, AudioSampleId, Mp4AudioTrack, PcmFormat, TimeStretcher};

pub use clock::{
    MediaClock, PLAYBACK_RATE_AUDIO_MAX, PLAYBACK_RATE_MAX, PLAYBACK_RATE_MIN,
    PLAYBACK_RATE_REVIEW_MAX, PLAYBACK_RATE_SHUTTLE_MIN, PLAYBACK_RATE_STEP, is_shuttle_rate,
    is_wall_clock_rate, normalize_playback_rate,
};
#[cfg(not(target_arch = "wasm32"))]
pub use demux::open_mp4_path;
pub use demux::{
    Mp4VideoTrack, OpenMp4Options, OpenedMp4, TopLevelBox, load_ftyp_moov, open_mp4,
    open_mp4_bytes, parse_mp4_progressive, parse_mp4_progressive_with_progress,
    read_ftyp_moov_stub, scan_top_level,
};
pub use error::{DecodeStall, MediaError};
pub use frame_index::{FrameEntry, FrameIndex};
// Host OpQueue / FSM — used by `fpmedia-web-components`; not part of the engine
// consumer surface (VideoPlayer / demux / sources).
#[doc(hidden)]
pub use epoch::{Epoch, Ticket};
#[doc(hidden)]
pub use host::host_op_coalesce::{HostOp, apply_enqueue};
#[doc(hidden)]
pub use host::open_phase::{OpenPhase, OpenSourceKind};
#[doc(hidden)]
pub use model::bg_task::{FlightGuard, SingleFlight};
#[doc(hidden)]
pub use model::clock_state::{
    ClockCmd, ClockEvent, ClockModel, ClockState, reduce as clock_state_reduce,
};
#[doc(hidden)]
pub use model::decoder_slot::{
    DecoderSlotCmd, DecoderSlotEvent, DecoderSlotModel, DecoderSlotPhase,
    reduce as decoder_slot_reduce,
};
#[doc(hidden)]
pub use model::engine_mode::{
    AudioResume, EngineCmd, EngineEvent, EngineMode, EngineModel, reduce as engine_mode_reduce,
};
#[doc(hidden)]
pub use model::play_phase::{PlayPhase, PlayPhaseEvent, SeamLatch, reduce as play_phase_reduce};
#[doc(hidden)]
pub use model::session::{SessionEvent, SessionModel, reduce as session_reduce};
#[doc(hidden)]
pub use model::transport::{
    TransportCmd, TransportEvent, TransportModel, TransportState, reduce as transport_reduce,
};
pub use policy::playback::{
    EofWall, LOOP_ORIGIN_PREWARM_SECS, LoopZone, eof_timeline_end, eof_wall_decision,
    loop_zone_wall_end, should_prewarm_loop_origin,
};
pub use policy::scrub_plan::{
    NextGapDecision, covered_aim_band, covered_edge_margins, decide_next_gap,
};
pub use policy::workload::{
    FRAGMENT_BACKFILL_IDLE_AHEAD, PLAY_THUMB_AIM_DECODE_MIN_MS, WorkloadHints, WorkloadMode,
    allows_bg_sync_verify, allows_thumb_aim_decode, allows_thumb_key_fill, classify,
    fragment_backfill_should_idle_yield,
};
pub use policy::{
    COVERED_EDGE_MARGIN_FRAMES, DEFAULT_SCRUB_BLIT_MIB, DEFAULT_SCRUB_CACHE_MIB,
    DEFAULT_THUMB_CACHE_MIB, EDGE_MARGIN_LEFT_MAX, EDGE_MARGIN_LEFT_MIN, EDGE_MARGIN_RIGHT_MIN,
    GopProfile, MAX_SCRUB_BLIT_FRAMES, MAX_SCRUB_CACHE_FRAMES, MAX_THUMB_CACHE_FRAMES,
    MIN_SCRUB_BLIT_FRAMES, MIN_SCRUB_CACHE_FRAMES, MIN_THUMB_CACHE_FRAMES,
    PLAY_PRESENTATION_CAP_HD, PLAY_PRESENTATION_CAP_UHD, PLAY_WARM_AHEAD_HD, PLAY_WARM_AHEAD_UHD,
    PlayPolicy, ResolutionClass, SCRUB_BLIT_LRU, SCRUB_DIR_BIAS_MAX_LAG_FRAMES,
    SETTLE_DENSIFY_YIELD_MS, SETTLE_DENSIFY_YIELD_RETRY_MS, SETTLE_GOP_SAMPLE_LEAD_MAX,
    SETTLE_GOP_SAMPLE_LEAD_MIN, SETTLE_PLAY_INDEX_AWAIT_BATCH, SETTLE_PLAY_INDEX_LEAD,
    SETTLE_TAKE_DECODER_WAIT_MS, SHORT_GOP_MAX_LEN, ScrubCachePolicy, ScrubDir, ScrubPolicy,
    SettlePolicy, ThumbPolicy, UHD_MIN_HEIGHT, estimated_decoded_frame_bytes,
    exact_frames_to_free_for_play_vram, hover_preview_choice, hover_thumb_dims,
    nearer_gap_with_dir, next_missing_sync_key, next_progressive_sync_key,
    scrub_blit_cap_during_play, scrub_blit_frame_budget, scrub_cache_frame_budget,
    scrub_drag_key_advances, scrub_drag_stage_choice, scrub_drag_stage_near_aim,
    scrub_edge_margins, scrub_gap_fill_armed, scrub_merge_dir_cached, scrub_preview_choice,
    scrub_preview_choice_with_dir, scrub_preview_neighbor, thumb_cache_frame_budget,
    thumb_progressive_coarse_count, thumb_progressive_rank,
};
pub use residency::AimWindow;
#[cfg(all(target_arch = "wasm32", feature = "webcodecs"))]
pub use source::{BlobSource, UrlSource, UrlSourceOptions, is_no_http_range};
pub use source::{
    CachedSource, DEFAULT_CACHE_BUDGET, DEFAULT_CACHE_SIZE, DEFAULT_URL_PARALLELISM, FetchMap,
    LOCAL_CACHE_BUDGET, MediaSource, MemorySource, OPEN_URL_PARALLELISM, ReadFuture,
};
pub use types::{
    AudioCodec, AudioCodecConfig, AudioMetadata, CodecConfig, EncodedAudioSample, EncodedSample,
    FrameId, FrameRun, Timestamp, VideoCodec, VideoMetadata, build_aac_asc, coalesce_frame_runs,
    merge_frame_runs,
};

#[cfg(all(target_arch = "wasm32", feature = "webcodecs"))]
pub use decode::{
    AsVideoFrame, OwnedVideoFrame, PLACEHOLDER_CANVAS_H, PLACEHOLDER_CANVAS_W, WebCodecsDecoder,
    canvas_2d, configure_canvas_for_video, draw_frame_to_canvas, draw_frame_with_context,
    reset_canvas_layout,
};
#[cfg(all(target_arch = "wasm32", feature = "webcodecs", feature = "thumbs"))]
pub use decode::{
    DEFAULT_THUMB_MAX_H, DEFAULT_THUMB_MAX_W, ThumbSink, ThumbSinkOptions, paint_thumb_canvas,
};
#[cfg(all(target_arch = "wasm32", feature = "webcodecs"))]
pub use video_player::{
    DuePresent, PipelineInterest, PipelineStats, PlayTick, PlayWaits, PlayerState, ReadyFrameKind,
    ReadyFrameMark, VideoPlayer,
};
