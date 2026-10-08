//! Playhead residency + densify/clock latch bag for [`super::VideoPlayer`].
//!
//! Split from `mod.rs` for navigation only — no behavior change (P0).
//! Fields are `pub(super)` so `play` / `scrub` / `seek` can touch interior state.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::rc::Rc;

use web_sys::{CanvasRenderingContext2d, HtmlCanvasElement};

use crate::MediaClock;
use crate::audio::AudioOutput;
use crate::clock_state::ClockState;
use crate::decode::frame_cache::{ScrubBlitLru, ScrubChunkStore};
use crate::decode::owned_video_frame::OwnedVideoFrame;
use crate::decode::scrub_blit::BlitCanvasSlot;
#[cfg(feature = "thumbs")]
use crate::decode::thumb_preview::ThumbPreview;
use crate::decode::webcodecs::WebCodecsDecoder;
use crate::decoder_slot::DecoderSlotModel;
use crate::demux::Mp4VideoTrack;
use crate::engine_mode::EngineModel;
use crate::error::MediaError;
use crate::policy::ThumbPolicy;
use crate::session::SessionModel;
use crate::types::{FrameId, Timestamp};

use super::api::PlayerState;
use crate::workload::WorkloadHints;

/// Live VF ahead of the playhead (play prefetch + Next hits).
/// Keep small on Hd — each slot pins a `VideoFrame`; 16 + in-flight decode starved
/// Chromium's pool on long-GOP play (`flush/idle` mid-GOP with `decodeQ≈12`).
/// Pair with [`STEP_BEHIND_CAP`] (ring ≈ 1 displayed + 8 behind + 4 ahead).
/// Hd default = [`crate::PLAY_PRESENTATION_CAP_HD`]; Uhd uses [`PlayPolicy`].
pub(crate) const PRESENTATION_QUEUE_CAP: usize = crate::PLAY_PRESENTATION_CAP_HD;
/// Ahead depth while shuttling forward (`rate >= 2`) — keep decode light.
pub(super) const SHUTTLE_PRESENTATION_QUEUE_CAP: usize = 2;
/// Live VF older than the playhead for Prev without key→N (step/scrub ring).
/// mediabunny reverse always restarts from key unless samples are retained —
/// this is the SampleCursor-lite behind window (`media-sink.ts` flush on rewind).
/// **Play must not keep this ring** — 1+8+4 + held≈8 pins ~21 VideoFrames and
/// starves Chromium's pool (underrun hold ≈0.77× realtime on long-GOP). Forward play
/// uses cap 0; Pause/scrub restores [`STEP_BEHIND_CAP`].
pub(crate) const STEP_BEHIND_CAP: usize = 8;
/// Behind ring while play mode — drop aged VF immediately.
pub(super) const STEP_BEHIND_CAP_PLAY: usize = 0;
/// Extra in-flight / held VF beyond displayed + presentation queue when sizing
/// Exact trim on Play (YUV↔RGBA heuristic headroom).
pub(super) const PLAY_EXACT_VRAM_VF_SLACK: usize = 4;
/// Ceiling on near-EOF presentation growth (live VF VRAM).
pub(super) const LOOP_NEAR_END_PRESENTATION_CAP_MAX: usize = 64;
/// Wall-clock lag above this → skip to nearest keyframe instead of exact frame.
pub(super) const BIG_LAG_SECONDS: f64 = 0.5;
/// Stop prefetching this many ms before the present deadline.
pub(super) const PREFETCH_DEADLINE_SLACK_MS: f64 = 3.0;

pub(super) struct QueuedFrame {
    pub(super) frame: FrameId,
    pub(super) video: OwnedVideoFrame,
}

/// Single playhead cache — fill policy lives in [`EngineModel`].
pub(super) struct Shared {
    pub(super) track: Rc<Mp4VideoTrack>,
    /// Committed playhead (last presented / settled frame).
    pub(super) frame: Cell<FrameId>,
    /// Host-visible play flag. Not derived from [`EngineModel`] — mode can be
    /// `ExactHold` / `Scrub` mid-play.
    pub(super) playing: Cell<bool>,
    /// HTML `<video loop>`.
    pub(super) looping: Cell<bool>,
    /// Ahead of playhead (presentation order). Cap [`PRESENTATION_QUEUE_CAP`].
    pub(super) presentation: RefCell<VecDeque<QueuedFrame>>,
    /// VF currently matching canvas / committed playhead (not yet in behind).
    pub(super) displayed: RefCell<Option<QueuedFrame>>,
    /// Older than displayed, oldest at front. Cap [`STEP_BEHIND_CAP`].
    pub(super) step_behind: RefCell<VecDeque<QueuedFrame>>,
    /// Exact scrub store (resident markers) — densify / seek / step gap geometry.
    pub(super) scrub_chunks: RefCell<ScrubChunkStore>,
    /// Live blit LRU for scrub present (`createImageBitmap`).
    pub(super) scrub_blit: RefCell<ScrubBlitLru>,
    /// Reusable OffscreenCanvas for VideoFrame → ImageBitmap.
    pub(super) encode_canvas: BlitCanvasSlot,
    /// Hover / filmstrip sink (`ThumbPolicy::On` → lazy create). Feature `thumbs`.
    /// `Rc` so sparse fill can hold the sink across `.await` without `RefCell` borrow.
    #[cfg(feature = "thumbs")]
    pub(super) thumb: RefCell<Option<Rc<ThumbPreview>>>,
    #[cfg(feature = "thumbs")]
    pub(super) thumb_policy: Cell<ThumbPolicy>,
    /// Second decoder for background gap fill (does not steal main playback).
    pub(super) scrub_decoder: RefCell<Option<WebCodecsDecoder>>,
    /// Densify create failed permanently this clip (e.g. HEVC HW-only — no SW
    /// second decoder). Stops retry storms that would call `prefer-hardware`.
    pub(super) scrub_densify_blocked: Cell<bool>,
    pub(super) scrub_epoch: Cell<u64>,
    pub(super) scrub_busy: Cell<bool>,
    /// Set while a fill is running so `schedule_scrub_cache_fill` can request another
    /// pass (avoids dropping work when pause/scrub bumps epoch mid-fill).
    pub(super) scrub_pending: Cell<bool>,
    /// Independent FetchMap (gray strip) hole fill — must not latch `scrub_busy`
    /// (densify abort / Exact yield must stay free of gray meter work).
    pub(super) gray_fill_busy: Cell<bool>,
    /// Finger on scrubber (`note_scrub_frame`) — throttle gray fill (hover-like).
    pub(super) scrub_gesture: Cell<bool>,
    /// Play vs scrub densify / exact hold + soft-hold audio.
    pub(super) engine: Cell<EngineModel>,
    /// Start frames decoded ahead of the loop seam (same decoder, while end plays).
    pub(super) loop_prime: RefCell<Option<VecDeque<QueuedFrame>>>,
    pub(super) canvas: HtmlCanvasElement,
    pub(super) ctx: CanvasRenderingContext2d,
    pub(super) frames_skipped: Cell<u64>,
    pub(super) last_behind_ms: Cell<Option<f64>>,
    /// Play underrun holds (clock freeze edges) — diagnose ~0.9× realtime.
    pub(super) underrun_holds: Cell<u64>,
    /// Accumulated wall ms spent with media clock frozen for underrun.
    pub(super) underrun_hold_ms: Cell<f64>,
    /// `performance.now` when the current underrun hold started (`None` = not holding).
    pub(super) underrun_hold_since_ms: Cell<Option<f64>>,
    /// Timeline phase ([`ClockState`]) — run / stop / underrun / scrub hold.
    pub(super) clock_state: Cell<ClockState>,
    /// Prefetch / seek-loop gates ([`SessionModel`]).
    pub(super) session: Cell<SessionModel>,
    /// Bumped to cancel in-flight decode (mediabunny dispose / asyncId).
    pub(super) op_epoch: Cell<u64>,
    /// Exclusive main-decoder checkout + reset-pending ([`DecoderSlotModel`]).
    pub(super) decoder_slot: Cell<DecoderSlotModel>,
    /// After fill stalls (no progress), skip restart until aim leaves this frame.
    pub(super) fill_stalled_aim: Cell<Option<FrameId>>,
    /// Scrub settle / seek landed on a cold (unindexed or uncached) frame — soft
    /// present now; densify paints Exact when bytes arrive (`pending_cold_exact`).
    pub(super) pending_cold_exact: Cell<Option<FrameId>>,
    /// Scrub drag direction for predictive densify: `+1` ahead, `-1` behind, `0` neutral.
    pub(super) scrub_dir: Cell<i8>,
    /// Last aim seen while updating `scrub_dir` (drag samples only).
    pub(super) prev_scrub_aim: Cell<Option<FrameId>>,
    /// Optional AAC (etc.) sink — drives timeline when present and playing.
    pub(super) audio: RefCell<Option<Rc<AudioOutput>>>,
    /// Silent `AudioContext` timeline when there is no AAC track
    /// (`mediabunny: media-player getPlaybackTime` always uses AudioContext).
    pub(super) playback_clock: Option<Rc<crate::audio::PlaybackClock>>,
    pub(super) clock: RefCell<MediaClock>,
    /// Wall `performance.now()/1000` origin — shuttle `>4×` only (or paused snaps).
    pub(super) wall_origin: Cell<Option<f64>>,
    /// File-byte fetch coverage (demo scrubber gray strip).
    pub(super) fetch_map: std::sync::Arc<crate::FetchMap>,
    pub(super) file_size: u64,
    /// Clip teardown latch — densify / backfill / thumb must exit (not adopt epoch).
    /// `mediabunny: examples/media-player` `asyncId++` before new `Input`.
    pub(super) disposed: Cell<bool>,
}

impl Shared {
    pub(super) fn workload_hints(&self) -> WorkloadHints {
        WorkloadHints::new(
            self.engine.get().mode,
            self.session.get().is_restarting(),
            // Underrun / scrub soft-hold only — not HoldHidden (tab freeze ≠ underrun).
            self.clock_state.get().blocks_bg_sync_verify(),
            self.scrub_busy.get(),
        )
    }

    #[inline]
    pub(super) fn is_disposed(&self) -> bool {
        self.disposed.get()
    }

    pub(super) fn state(&self) -> PlayerState {
        let frame = self.frame.get();
        let timestamp = self
            .track
            .index()
            .get(frame)
            .map(|e| e.composition_timestamp())
            .unwrap_or(Timestamp::from_seconds(0.0));
        PlayerState {
            frame,
            timestamp,
            playing: self.playing.get(),
            looping: self.looping.get(),
            frame_count: self.track.frame_count(),
        }
    }

    /// Commit the playhead to `frame` (after a successful decode / present).
    /// Does not touch [`Self::playing`] — the play loop relies on that.
    pub(super) fn commit_frame(&self, frame: FrameId) -> Result<(), MediaError> {
        let count = self.track.frame_count();
        if frame >= count {
            return Err(MediaError::FrameOutOfRange(frame, count));
        }
        self.frame.set(frame);
        Ok(())
    }
}
