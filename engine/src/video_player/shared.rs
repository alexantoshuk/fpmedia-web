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
use crate::decode::frame_cache::ScrubBlitLru;
use crate::decode::owned_video_frame::OwnedVideoFrame;
use crate::decode::scrub_blit::BlitCanvasSlot;
#[cfg(feature = "thumbs")]
use crate::decode::thumb_sink::ThumbSink;
use crate::decode::webcodecs::{PlayIter, WebCodecsDecoder};
use crate::demux::Mp4VideoTrack;
use crate::error::MediaError;
use crate::model::bg_task::SingleFlight;
use crate::model::clock_state::ClockState;
use crate::model::decoder_slot::DecoderSlotModel;
use crate::model::engine_mode::EngineModel;
use crate::model::play_phase::PlayPhase;
use crate::model::session::SessionModel;
use crate::policy::{ScrubCachePolicy, ThumbPolicy};
use crate::residency::ScrubChunkStore;
use crate::types::{FrameId, Timestamp};

use super::api::PlayerState;
use super::play_pool::PlayCanvasPool;
use crate::policy::workload::WorkloadHints;

/// Live VF ahead of the playhead for paused Next hits (not used by Play).
/// Pair with [`STEP_BEHIND_CAP`] (ring ≈ 1 displayed + 8 behind + 4 ahead).
/// Hd default = [`crate::PLAY_PRESENTATION_CAP_HD`]; Uhd uses [`PlayPolicy`].
pub(crate) const PRESENTATION_QUEUE_CAP: usize = crate::PLAY_PRESENTATION_CAP_HD;
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

pub(super) struct QueuedFrame {
    pub(super) frame: FrameId,
    pub(super) video: OwnedVideoFrame,
}

/// Decoded Play frame waiting for its timestamp (mediabunny `nextFrame`).
pub(super) struct PlayNext {
    pub(super) frame: FrameId,
    pub(super) ts: f64,
    /// [`PlayCanvasPool`] slot holding the pixels.
    pub(super) slot: usize,
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
    /// Product: [`crate::ThumbSink`] — also usable without this player.
    #[cfg(feature = "thumbs")]
    pub(super) thumb: RefCell<Option<Rc<ThumbSink>>>,
    #[cfg(feature = "thumbs")]
    pub(super) thumb_policy: Cell<ThumbPolicy>,
    /// Second decoder for background gap fill (does not steal main playback).
    pub(super) scrub_decoder: RefCell<Option<WebCodecsDecoder>>,
    /// `Off` → no scrub decoder / Covered / Exact / gray fill (host without scrub UI).
    pub(super) scrub_cache_policy: Cell<ScrubCachePolicy>,
    /// Densify create failed permanently this clip (e.g. HEVC HW-only — no SW
    /// second decoder). Stops retry storms that would call `prefer-hardware`.
    pub(super) scrub_densify_blocked: Cell<bool>,
    pub(super) scrub_epoch: Cell<u64>,
    /// Densify single-flight (`busy` + `pending` restart).
    pub(super) densify: SingleFlight,
    /// Independent FetchMap (gray strip) hole fill — must not share densify flight
    /// (Exact yield must stay free of gray meter work).
    pub(super) gray_fill: SingleFlight,
    /// Document hidden — yield densify / gray / thumbs; Play clock keeps running.
    /// `mediabunny: examples/media-player` — no visibility hold; local scrub-plane gate.
    pub(super) scrub_bg_suspended: Cell<bool>,
    /// After tab visible: `performance.now` until which video catch-up must not
    /// [`super::VideoPlayer::hold_media_clock_for_underrun`] (micro audio pause).
    pub(super) underrun_catchup_until_ms: Cell<Option<f64>>,
    /// Near-EOF / seam already kicked loop-origin byte warm this lap (clear on
    /// [`PlayPhase::LoopDone`] / session end).
    pub(super) loop_origin_warm_started: Cell<bool>,
    /// Finger on scrubber (`note_scrub_frame`) — throttle gray fill (hover-like).
    pub(super) scrub_gesture: Cell<bool>,
    /// Play vs scrub densify / exact hold + soft-hold audio.
    pub(super) engine: Cell<EngineModel>,
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
    /// Seek / scrub restart latch ([`SessionModel`]).
    pub(super) session: Cell<SessionModel>,
    /// Play session lifetime + loop seam ([`PlayPhase`]).
    pub(super) play_phase: Cell<PlayPhase>,
    /// Continuous Play range — owns the main decoder while `Some`.
    pub(super) play_iter: RefCell<Option<PlayIter>>,
    /// mediabunny `nextFrame` (pixels in [`Self::play_pool`]).
    pub(super) play_next: RefCell<Option<PlayNext>>,
    /// Lazily created on first Play.
    pub(super) play_pool: RefCell<Option<PlayCanvasPool>>,
    /// `updateNextFrame` single-flight.
    pub(super) play_updating: Cell<bool>,
    /// No-AAC: arm silent [`crate::audio::PlaybackClock`] on first present tick.
    /// `mediabunny: media-player play` / `audioContextStartTime`.
    pub(super) pending_silent_clock: Cell<bool>,
    /// Warm clock resume in flight — do not stack.
    pub(super) resuming_clock: Cell<bool>,
    /// Media time snapped at underrun hold for warm resume.
    pub(super) held_media_origin: Cell<Option<f64>>,
    /// Iterator / loop async failure for the next [`super::VideoPlayer::play_raf_tick`].
    pub(super) play_driver_error: RefCell<Option<String>>,
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
            // Underrun / scrub soft-hold only (tab-hidden yields scrub planes, not clock).
            self.clock_state.get().blocks_bg_sync_verify(),
            self.densify.is_busy(),
        )
    }

    #[inline]
    pub(super) fn is_disposed(&self) -> bool {
        self.disposed.get()
    }

    #[inline]
    pub(super) fn scrub_cache_off(&self) -> bool {
        self.scrub_cache_policy.get() == ScrubCachePolicy::Off
    }

    #[inline]
    pub(super) fn scrub_bg_suspended(&self) -> bool {
        self.scrub_bg_suspended.get()
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
