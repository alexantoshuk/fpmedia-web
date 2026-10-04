//! Browser video player façade: demux + WebCodecs decode + canvas (+ optional audio).
//!
//! Decode plumbing lives in [`crate::decode`]; this module owns play/scrub/seek orchestration.
//! Present and decode share the player via interior mutability so an rAF present loop
//! never waits on `prefetch_one` (mediabunny-style split).

use std::cell::RefCell;
use std::rc::Rc;

use web_sys::HtmlCanvasElement;

use crate::decode::webcodecs::WebCodecsDecoder;
use crate::engine_mode::{EngineEvent, reduce as engine_reduce};
use crate::player::PlayerState;
use crate::session::{SessionEvent, reduce as session_reduce};
use crate::types::{AudioMetadata, FrameId, GopProfile, VideoMetadata};

mod play;
mod scrub;
mod scrub_cold;
mod scrub_gap;
mod clock_ctrl;
mod present;
mod decode_ops;
mod open;
mod seek;
mod shared;
mod util;

use scrub::scrub_cache_fill_task;
use scrub_gap::next_gap_target;

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

impl VideoPlayer {
    pub fn metadata(&self) -> VideoMetadata {
        self.shared.player.borrow().metadata()
    }

    pub fn audio_metadata(&self) -> Option<AudioMetadata> {
        self.shared.audio.borrow().as_ref().map(|a| a.metadata())
    }

    pub fn has_audio(&self) -> bool {
        self.shared.audio.borrow().is_some()
    }


    fn apply_session_event(&self, event: SessionEvent) {
        let next = session_reduce(self.shared.session.get(), event);
        self.shared.session.set(next);
    }


    pub fn state(&self) -> PlayerState {
        self.shared.player.borrow().state()
    }

    /// Frames currently in the demux index (fMP4 warm prefix may be ≪ duration).
    pub fn indexed_frame_count(&self) -> FrameId {
        self.shared.player.borrow().track().indexed_frame_count()
    }

    /// True when Exact settle/seek may cascade without cold Range stampede.
    ///
    /// Host PlayPrime snaps playhead here when UI aim is still gray/unindexed.
    pub fn frame_exact_ready(&self, frame: FrameId) -> bool {
        scrub_cold::exact_scrub_ready(&self.shared, frame)
    }

    pub fn canvas(&self) -> &HtmlCanvasElement {
        &self.shared.canvas
    }

    /// Recompute canvas buffer from current CSS layout × DPR (call on container resize).
    ///
    /// Changing `canvas.width`/`height` clears the bitmap (fullscreen ↔ windowed).
    /// Keep scrub **markers** + **ImageBitmap** LRU — they scale via `drawImage` dw/dh
    /// (old WebP still-store needed a wipe; blit does not). `mediabunny:` no densify
    /// wipe on resize; CanvasSink-style present redraw.

    pub fn is_playing(&self) -> bool {
        self.state().playing
    }

    pub fn play(&self) {
        let (next, _) = engine_reduce(self.shared.engine.get(), EngineEvent::EnterPlay);
        self.shared.engine.set(next);
        // Cancel densify hard — do **not** rearm `scrub_pending` (Play must not
        // keep filling gaps). Keep scrub *markers*; trim Exact (ImageBitmap) by
        // Play VF VRAM estimate — evict farthest from playhead, do not wipe all
        // (was: full clear → cold Exact on every Pause).
        self.cancel_ops();
        self.abort_scrub_fill(false);
        self.clear_scrub_direction();
        self.trim_exact_for_play();
        // Free Chromium VideoFrame pool for prefetch — step ring is Pause/Prev only.
        self.clear_step_behind_ring();
        {
            let player = self.shared.player.borrow();
            let head = player.state().frame;
            let track = player.track_rc();
            if !track.fragments_complete() {
                // Pump mfra index well ahead of play so we never stall at the
                // warm+chunk seam (was solid lag on frame 119 / indexed 120).
                track.request_index_through(head.saturating_add(240));
            }
            // Warm `mdat` for the next ~5s even when the index is already ahead
            // (mfra backfill does not fetch sample bytes).
            track.prefetch_samples_ahead(head, 120);
        }
        self.shared.player.borrow_mut().play();
        self.shared.frames_skipped.set(0);
        self.shared.last_behind_ms.set(None);
        self.apply_clock_event(ClockEvent::ResetForPlay);
        self.shared.underrun_holds.set(0);
        self.shared.underrun_hold_ms.set(0.0);
        // Do **not** schedule_thumb_key_fill here — Play owns Ranges/main thread
        // for sparse chains; hover uses throttled single-aim decode instead.
        // Fill resumes on Pause / scrub (`allows_thumb_key_fill`).
    }

    pub fn pause(&self) {
        self.pause_inner(false);
    }

    /// Leave play with soft-held audio (no `AudioDecoder` reset) — Pause button
    /// and scrub-while-play.
    ///
    /// Hard [`Self::pause`] still tears down audio (open / EOF / abort-prime).
    /// `mediabunny: examples/media-player/media-player.ts` `pause` / `play`
    /// (nodes + iterator only; no reconfigure).
    pub fn pause_for_scrub(&self) {
        self.pause_inner(true);
    }

    fn pause_inner(&self, soft_audio: bool) {
        // Leaving play: cancel decode + scrub fill epoch (pending latch restarts fill).
        // Already paused: do not bump scrub_epoch (UI steps call pause every time).
        let was_playing = self.shared.player.borrow().state().playing;
        let (next, _) = engine_reduce(
            self.shared.engine.get(),
            EngineEvent::EnterScrub { soft_audio },
        );
        self.shared.engine.set(next);
        if was_playing {
            self.cancel_ops_and_scrub_fill();
            if soft_audio {
                let _ = self.hold_media_clock_for_scrub();
            } else {
                let _ = self.stop_media_clock();
            }
        }
        self.shared.player.borrow_mut().pause();
        self.clear_loop_prime();
        // `prepare_scrub_settle` latches ExactHold *before* the async scrub op;
        // `run_seek_or_step` always called `pause()` which used to clear that latch
        // and re-kick densify — exact settle then raced the scrub decoder
        // (`decoder busy` / play-after-scrub resume fail on URL fMP4).
        if self.shared.engine.get().holds_exact() {
            return;
        }
        // Same pipeline as scrub drag / hold / release.
        self.enter_scrub_cache_pipeline(self.state().frame);
        if was_playing {
            // Cancelled fill may still have `scrub_busy` until its finally runs;
            // schedule only latches pending then. Kick again after a turn so pause
            // densify is not lost when the busy race clears without want_again.
            self.shared.scrub_pending.set(true);
            let shared = Rc::clone(&self.shared);
            wasm_bindgen_futures::spawn_local(async move {
                yield_event_loop().await;
                if shared.is_disposed()
                    || shared.engine.get().holds_exact()
                    || shared.engine.get().is_play()
                    || shared.player.borrow().state().playing
                    || !shared.player.borrow().metadata().scrub_policy().densify
                {
                    return;
                }
                if shared.scrub_busy.get() {
                    shared.scrub_pending.set(true);
                    return;
                }
                if next_gap_target(&shared).is_none() {
                    return;
                }
                shared.scrub_busy.set(true);
                shared.scrub_pending.set(false);
                let again = Rc::clone(&shared);
                let _ = scrub_cache_fill_task(again).await;
            });
        }
    }

    /// Bump the op epoch so in-flight decode/seek ignores its result.
    /// Does **not** cancel background scrub fill — scrub/seek call [`Self::begin_op`]
    /// often; killing fill on every pointermove left the cache stuck on the old GOP.
    pub fn cancel_ops(&self) {
        let next = self.shared.op_epoch.get().wrapping_add(1);
        self.shared.op_epoch.set(next);
    }

    /// Cancel decode **and** background dense fill (play / hard abort).
    pub fn cancel_ops_and_scrub_fill(&self) {
        self.cancel_ops();
        self.abort_scrub_fill(true);
        self.clear_scrub_direction();
    }

    fn begin_op(&self) -> u64 {
        self.cancel_ops();
        self.shared.op_epoch.get()
    }

    fn is_current(&self, epoch: u64) -> bool {
        self.shared.op_epoch.get() == epoch
    }

    pub fn set_loop(&self, looping: bool) {
        self.shared.player.borrow_mut().set_loop(looping);
        if !looping {
            self.clear_loop_prime();
        }
    }

    pub fn is_looping(&self) -> bool {
        self.shared.player.borrow().is_looping()
    }

    pub fn is_restarting(&self) -> bool {
        self.shared.session.get().is_restarting()
    }

}

impl Drop for VideoPlayer {
    fn drop(&mut self) {
        // Hard clip teardown — densify used to adopt scrub_epoch bumps and keep
        // `Shared` (+ AudioContext) alive across open_url; Chrome then goes silent
        // after ~6 contexts and lag compounds.
        // mediabunny: initMediaPlayer pause → iterator.return → asyncId++ / UrlSource._dispose.
        self.shared.disposed.set(true);
        self.cancel_ops();
        self.shared.scrub_pending.set(false);
        // Stop AAC pump + drop sink so `AudioContext.close` runs even if a late
        // densify turn still holds `Shared` for a moment.
        if let Ok(mut slot) = self.shared.audio.try_borrow_mut()
            && let Some(a) = slot.take()
        {
            let _ = a.stop();
            drop(a);
        }
        if let Some(c) = self.shared.playback_clock.as_ref() {
            c.close();
        }
        // Cancel epoch + drop decoder/cache before releasing the sink Rc so
        // in-flight aim/fill see generation bump (not only Shared.disposed).
        #[cfg(feature = "thumbs")]
        if let Ok(mut slot) = self.shared.thumb.try_borrow_mut() {
            if let Some(thumbs) = slot.as_ref() {
                thumbs.clear();
            }
            *slot = None;
        }
        self.clear_loop_prime();
        self.clear_presentation();
        self.clear_step_ring();
        // Bumps scrub_epoch + drops scrub decoder + clears Covered/Exact.
        self.clear_scrub_store();
        // Abort Range workers now — densify/backfill may keep `Shared` a few turns.
        if let Ok(p) = self.shared.player.try_borrow() {
            p.track_rc().source().dispose();
        }
    }
}
