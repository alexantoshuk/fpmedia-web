//! Browser video player façade: demux + WebCodecs decode + canvas (+ optional audio).
//!
//! Decode plumbing lives in [`crate::decode`]; this module owns play/scrub/seek orchestration.
//! Present and decode share the player via interior mutability so an rAF present loop
//! never waits on `prefetch_one` (mediabunny-style split).

use std::cell::RefCell;
use std::rc::Rc;

use crate::decode::webcodecs::WebCodecsDecoder;
use crate::types::{FrameId, GopProfile};

mod play;
mod scrub;
mod scrub_cold;
mod scrub_gap;
mod clock_ctrl;
mod present;
mod decode_ops;
mod open;
mod control;
mod seek;
mod shared;
mod util;

pub use shared::{PRESENTATION_QUEUE_CAP, STEP_BEHIND_CAP};
// Private imports — child mods (`play` / `scrub` / `seek`) see these via `super::`.
use shared::{
    BIG_LAG_SECONDS, LOOP_NEAR_END_PRESENTATION_CAP_MAX, PLAY_EXACT_VRAM_VF_SLACK,
    PREFETCH_DEADLINE_SLACK_MS, QueuedFrame, SHUTTLE_PRESENTATION_QUEUE_CAP, Shared,
};
use util::{
    is_decoder_poison, performance_now_ms, sleep_ms, wait_animation_frame, yield_event_loop,
};

/// Why a frame is marked ready on the demo timeline strip.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReadyFrameKind {
    /// Scrub store marker (coverage / gap geometry). May lack live blit.
    Covered,
    /// Live densify `ImageBitmap` (step-ready).
    Exact,
}

/// Paint order for overlapping strips (Covered under Exact).
pub(super) fn ready_kind_paint_order(kind: ReadyFrameKind) -> u8 {
    match kind {
        ReadyFrameKind::Covered => 0,
        ReadyFrameKind::Exact => 1,
    }
}

/// One ready span for UI timeline marks (`frame`..=`end`, inclusive).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReadyFrameMark {
    /// Inclusive start (presentation frame).
    pub frame: FrameId,
    /// Inclusive end (`end >= frame`). Single tick when equal to `frame`.
    pub end: FrameId,
    pub kind: ReadyFrameKind,
}

impl ReadyFrameMark {
    pub fn run(lo: FrameId, hi: FrameId, kind: ReadyFrameKind) -> Self {
        Self {
            frame: lo,
            end: hi.max(lo),
            kind,
        }
    }
}

/// Live decode / prefetch counters for the demo UI (and host apps).
#[derive(Clone, Debug, PartialEq)]
pub struct PipelineStats {
    pub presentation_len: usize,
    pub presentation_cap: usize,
    pub presentation_from: Option<FrameId>,
    pub presentation_to: Option<FrameId>,
    pub held_len: usize,
    pub decode_queue_size: u32,
    pub frames_skipped: u64,
    pub last_behind_ms: Option<f64>,
    /// Underrun clock-freeze edges since last [`VideoPlayer::play`].
    pub underrun_holds: u64,
    /// Wall ms spent frozen for underrun since last play (includes in-progress hold).
    pub underrun_hold_ms: f64,
    pub cache_chunks: usize,
    pub cache_frames: usize,
    pub cache_cap: usize,
    /// Dense extent is a single contiguous run (gap-fill edge-armed only).
    pub cache_no_gaps: bool,
    pub scrub_busy: bool,
    /// Inclusive cache extent `[from, to]` (presentation frames), if any.
    pub scrub_window_from: Option<FrameId>,
    pub scrub_window_to: Option<FrameId>,
    /// Frames ready in cache / presentation queue (timeline strip). Sorted by frame.
    pub ready_frames: Vec<ReadyFrameMark>,
    /// Keyframe spacing class (scrub budget / fill policy).
    pub gop_profile: GopProfile,
    /// Bounded union of recent [`crate::CachedSource`] windows as file-size
    /// fractions (`0..1`) for the demo gray strip (not densify — green below).
    pub fetch_file_ranges: Vec<(f32, f32)>,
}

impl Default for PipelineStats {
    fn default() -> Self {
        Self {
            presentation_len: 0,
            presentation_cap: PRESENTATION_QUEUE_CAP,
            presentation_from: None,
            presentation_to: None,
            held_len: 0,
            decode_queue_size: 0,
            frames_skipped: 0,
            last_behind_ms: None,
            underrun_holds: 0,
            underrun_hold_ms: 0.0,
            cache_chunks: 0,
            cache_frames: 0,
            cache_cap: 0,
            cache_no_gaps: false,
            scrub_busy: false,
            scrub_window_from: None,
            scrub_window_to: None,
            ready_frames: Vec::new(),
            gop_profile: GopProfile::Long { median: 0, max: 0 },
            fetch_file_ranges: Vec::new(),
        }
    }
}

/// Result of a sync clock-driven present attempt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DuePresent {
    Playing { did_draw: bool },
    NeedsLoop,
    Ended,
}

/// Frame-accurate video player (WebCodecs → canvas); optional audio via [`crate::AudioOutput`].
/// Pure-audio hosts will use a separate `AudioPlayer` over the same audio modules.
pub struct VideoPlayer {
    shared: Rc<Shared>,
    /// `None` only while an async decode owns the decoder (must not re-enter).
    decoder: RefCell<Option<WebCodecsDecoder>>,
}
