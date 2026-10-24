//! Host-facing types for [`super::VideoPlayer`] (stats / strip / present result).
//!
//! Split from `mod.rs` for navigation only — no behavior change (P6).

pub use crate::decode::webcodecs::PlayWaits;
use crate::policy::GopProfile;
use crate::types::{FrameId, Timestamp};

use super::PRESENTATION_QUEUE_CAP;

/// Playhead snapshot ([`super::VideoPlayer::state`]).
#[derive(Clone, Debug, PartialEq)]
pub struct PlayerState {
    pub frame: FrameId,
    pub timestamp: Timestamp,
    pub playing: bool,
    /// When true, playback wraps to frame 0 at end (HTML `<video loop>`).
    pub looping: bool,
    pub frame_count: u64,
}

/// Why a frame is marked ready on the demo timeline strip.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReadyFrameKind {
    /// Sync / keyframe positions from the frame index (structural, not densify).
    Keyframes,
    /// Scrub store marker (coverage / gap geometry). May lack live blit.
    Covered,
    /// Live densify `ImageBitmap` (step-ready).
    Exact,
}

/// Paint order for overlapping strips (Keyframes under Covered under Exact).
pub(super) fn ready_kind_paint_order(kind: ReadyFrameKind) -> u8 {
    match kind {
        ReadyFrameKind::Keyframes => 0,
        ReadyFrameKind::Covered => 1,
        ReadyFrameKind::Exact => 2,
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

/// Which [`PipelineStats`] planes to fill — host interest from composed strips / debug UI.
///
/// Empty planes stay at [`Default`] (no coalesce / no FetchMap snap).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PipelineInterest {
    /// Gray FetchMap file-fraction ranges.
    pub fetch: bool,
    /// Keyframes / Covered / Exact ready marks.
    pub ready: bool,
    /// Decode / underrun / cache counters (debug panel).
    pub meters: bool,
}

impl PipelineInterest {
    pub const fn none() -> Self {
        Self {
            fetch: false,
            ready: false,
            meters: false,
        }
    }

    pub const fn any(self) -> bool {
        self.fetch || self.ready || self.meters
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
    /// Underrun clock-freeze edges since last [`super::VideoPlayer::play`].
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
    /// Timeline strip marks (Keyframes / Covered / Exact). Sorted by frame.
    pub ready_frames: Vec<ReadyFrameMark>,
    /// Keyframe spacing class (scrub budget / fill policy).
    pub gop_profile: GopProfile,
    /// Bounded union of recent [`crate::CachedSource`] windows as file-size
    /// fractions (`0..1`) for the demo gray strip (not densify — green below).
    pub fetch_file_ranges: Vec<(f32, f32)>,
    /// Play iterator wait meters (last ~1 s); `None` outside a play session.
    pub play_waits: Option<PlayWaits>,
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
            play_waits: None,
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

/// Host rAF outcome after [`super::VideoPlayer::play_raf_tick`].
///
/// Engine owns silent-clock arm, underrun hold/resume, and loop seam; the host
/// only paints UI / stops on EOF. `mediabunny: examples/media-player/media-player.ts`
/// `render` / `play` / `pause`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PlayTick {
    /// Restarting or session not live — skip UI this frame.
    Skipped,
    /// EOF with loop off — host should stop transport.
    Ended,
    /// Normal present turn.
    Presented { did_draw: bool },
    /// Prefetch / loop driver failed — host should surface and stop.
    Failed(String),
}
