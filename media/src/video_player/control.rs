//! Transport / session control surface for [`super::VideoPlayer`].
//!
//! Split from `mod.rs` for navigation only — no behavior change (P5).
//! `mediabunny:` play/pause / soft-hold / dispose (iterator.return + UrlSource).

use std::rc::Rc;

use web_sys::HtmlCanvasElement;

use super::api::PlayerState;
use crate::model::clock_state::ClockEvent;
use crate::model::engine_mode::{EngineCmd, EngineEvent, reduce as engine_reduce};
use crate::model::session::{SessionEvent, reduce as session_reduce};
use crate::types::{AudioMetadata, FrameId, VideoMetadata};

use super::scrub::scrub_cache_fill_task;
use super::scrub_cold;
use super::scrub_gap::next_gap_target;
use super::{VideoPlayer, yield_microtask};

impl VideoPlayer {
    pub fn metadata(&self) -> VideoMetadata {
        self.shared.track.metadata()
    }

    pub fn audio_metadata(&self) -> Option<AudioMetadata> {
        self.shared.audio.borrow().as_ref().map(|a| a.metadata())
    }

    pub fn has_audio(&self) -> bool {
        self.shared.audio.borrow().is_some()
    }

    pub(super) fn apply_session_event(&self, event: SessionEvent) {
        let next = session_reduce(self.shared.session.get(), event);
        self.shared.session.set(next);
    }

    /// `engine_mode::reduce` → store model → run [`EngineCmd`]s.
    ///
    /// `run_clock_cmds`: Pause only soft/hard-stops the clock when leaving play
    /// (`was_playing`); already-paused UI steps must not re-enter hold/stop.
    /// `mediabunny: examples/media-player/media-player.ts` seekToTime → pause/play.
    pub(super) fn apply_engine_event(&self, event: EngineEvent) {
        self.apply_engine_event_gated(event, true);
    }

    pub(super) fn apply_engine_event_gated(&self, event: EngineEvent, run_clock_cmds: bool) {
        let (next, cmds) = engine_reduce(self.shared.engine.get(), event);
        self.shared.engine.set(next);
        self.apply_engine_cmds(&cmds, run_clock_cmds);
    }

    pub(super) fn apply_engine_cmds(&self, cmds: &[EngineCmd], run_clock_cmds: bool) {
        for cmd in cmds {
            match *cmd {
                EngineCmd::AbortDensify { rearm } => self.abort_scrub_fill(rearm),
                EngineCmd::ScheduleDensify => {
                    // ExactHold / Play must not densify — reduce already omits this
                    // cmd there; `schedule_scrub_cache_fill` also refuses Play.
                    if self.shared.engine.get().allows_densify() {
                        self.enter_scrub_cache_pipeline(self.state().frame);
                    }
                }
                EngineCmd::SoftHoldClock if run_clock_cmds => {
                    let _ = self.hold_media_clock_for_scrub();
                }
                EngineCmd::HardStopClock if run_clock_cmds => {
                    let _ = self.stop_media_clock();
                }
                EngineCmd::SoftHoldClock | EngineCmd::HardStopClock => {}
            }
        }
    }

    pub fn state(&self) -> PlayerState {
        self.shared.state()
    }

    /// Frames currently in the demux index (fMP4 warm prefix may be ≪ duration).
    pub fn indexed_frame_count(&self) -> FrameId {
        self.shared.track.indexed_frame_count()
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

    pub fn is_playing(&self) -> bool {
        self.state().playing
    }

    pub fn play(&self) {
        // EnterPlay → AbortDensify { rearm: false }.
        self.apply_engine_event(EngineEvent::EnterPlay);
        // Release+then_play skips the cache pipeline — do not leave gray throttled.
        self.shared.scrub_gesture.set(false);
        // Keep scrub *markers*; trim Exact (ImageBitmap) by Play VF VRAM estimate —
        // evict farthest from playhead, do not wipe all (was: full clear → cold
        // Exact on every Pause).
        self.cancel_ops();
        self.clear_scrub_direction();
        self.trim_exact_for_play();
        // Free Chromium VideoFrame pool for prefetch — step ring is Pause/Prev only.
        self.clear_step_behind_ring();
        {
            let head = self.shared.frame.get();
            let track = &self.shared.track;
            if !track.fragments_complete() {
                // Pump mfra index well ahead of play so we never stall at the
                // warm+chunk seam (was solid lag on frame 119 / indexed 120).
                track.request_index_through(head.saturating_add(240));
            }
            // Warm `mdat` for the next ~5s even when the index is already ahead
            // (mfra backfill does not fetch sample bytes).
            track.prefetch_samples_ahead(head, 120);
        }
        self.shared.playing.set(true);
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
        // Cold audio + densify (open / EOF / abort-prime tear-down).
        self.pause_inner(false, true);
    }

    /// Leave play with soft-held audio — **no densify** (soft-drag under finger).
    ///
    /// Pause **button** uses [`Self::pause_warm_idle`] (warm + densify).
    /// Hard [`Self::pause`] tears down audio. `mediabunny: examples/media-player`
    /// `pause` / `play` (nodes + iterator only; no reconfigure).
    pub fn pause_for_scrub(&self) {
        self.pause_inner(true, false);
    }

    /// Pause button: warm audio resume + densify / gray fill (idle scrub residency).
    pub fn pause_warm_idle(&self) {
        self.pause_inner(true, true);
    }

    pub(super) fn pause_inner(&self, soft_audio: bool, densify: bool) {
        // Leaving play: cancel decode + scrub fill epoch (pending latch restarts fill).
        // Already paused: do not bump scrub_epoch (UI steps call pause every time).
        let was_playing = self.shared.playing.get();
        if was_playing {
            self.cancel_ops_and_scrub_fill();
        }
        // Flip playing before ScheduleDensify — `schedule_scrub_cache_fill` refuses
        // while playing. Clock cmds still gated by `was_playing`.
        self.shared.playing.set(false);
        self.clear_play_session();
        self.clear_loop_prime();
        self.apply_engine_event_gated(
            EngineEvent::EnterScrub {
                soft_audio,
                densify,
            },
            was_playing,
        );
        // `prepare_scrub_settle` latches ExactHold *before* the async scrub op;
        // `run_seek_or_step` always called `pause()` which used to clear that latch
        // and re-kick densify — exact settle then raced the scrub decoder
        // (`decoder busy` / play-after-scrub resume fail on URL fMP4).
        if self.shared.engine.get().holds_exact() {
            return;
        }
        // Soft-drag: aim only — PreferSoftware densify under the finger starves
        // pointer/rAF. Pause / hard idle densify via ScheduleDensify + re-kick.
        if !densify {
            self.aim_scrub_cache(self.state().frame);
            return;
        }
        if was_playing {
            // Cancelled fill may still have `scrub_busy` until its finally runs;
            // schedule only latches pending then. Kick again after a turn so pause
            // densify is not lost when the busy race clears without want_again.
            self.shared.densify.latch_pending();
            let shared = Rc::clone(&self.shared);
            wasm_bindgen_futures::spawn_local(async move {
                yield_microtask().await;
                if shared.is_disposed()
                    || shared.engine.get().holds_exact()
                    || shared.engine.get().is_play()
                    || shared.playing.get()
                    || !shared.track.metadata_ref().scrub_policy().densify
                {
                    return;
                }
                if shared.densify.is_busy() {
                    shared.densify.latch_pending();
                    // Green may already be saturated — still close gray holes.
                    scrub_cold::schedule_fetchmap_gap_fill(&shared);
                    return;
                }
                if next_gap_target(&shared).is_none() {
                    scrub_cold::schedule_fetchmap_gap_fill(&shared);
                    return;
                }
                if !shared.densify.request() {
                    return;
                }
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

    /// Cancel decode **and** background dense fill (play / hard abort / drag).
    pub fn cancel_ops_and_scrub_fill(&self) {
        self.cancel_ops();
        // Drag: rearm false so densify cannot adopt under the finger.
        self.abort_scrub_fill(false);
        self.clear_scrub_direction();
    }

    pub(super) fn begin_op(&self) -> u64 {
        self.cancel_ops();
        self.shared.op_epoch.get()
    }

    pub(super) fn is_current(&self, epoch: u64) -> bool {
        self.shared.op_epoch.get() == epoch
    }

    pub fn set_loop(&self, looping: bool) {
        self.shared.looping.set(looping);
        if !looping {
            self.clear_loop_prime();
        }
    }

    pub fn is_looping(&self) -> bool {
        self.shared.looping.get()
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
        self.shared.densify.clear_pending();
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
        self.drop_thumb_sink();
        self.clear_loop_prime();
        self.clear_presentation();
        self.clear_step_ring();
        // Bumps scrub_epoch + drops scrub decoder + clears Covered/Exact.
        self.clear_scrub_store();
        // Abort Range workers now — densify/backfill may keep `Shared` a few turns.
        self.shared.track.source().dispose();
    }
}
