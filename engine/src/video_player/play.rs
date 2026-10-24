//! Play iterator / render / loop orchestration for [`super::VideoPlayer`].
//!
//! `mediabunny: examples/media-player/media-player.ts` — `startVideoIterator`
//! (first frame drawn, second kept as `nextFrame`), `updateNextFrame` (draw
//! overdue samples, park the first future one), `render` (draw `nextFrame` once
//! due). Packets: [`PlayIter`] (`src/media-sink.ts mediaSamplesInRange`).
//! Pixels: [`super::play_pool::PlayCanvasPool`] (`CanvasSink` pool of 2).

use std::rc::Rc;

use crate::decode::canvas::draw_frame_with_context;
use crate::decode::webcodecs::PlayIter;
use crate::error::MediaError;
use crate::model::play_phase::{PlayPhase, PlayPhaseEvent, reduce as play_phase_reduce};
use crate::policy::playback::{EofWall, eof_timeline_end, eof_wall_decision};
use crate::policy::{
    exact_frames_to_free_for_play_vram, scrub_blit_cap_during_play, scrub_blit_frame_budget,
};
use crate::types::FrameId;

use super::play_pool::PlayCanvasPool;
use super::shared::PlayNext;
use super::{DuePresent, PLAY_EXACT_VRAM_VF_SLACK, PlayTick, VideoPlayer, now_ms};

impl VideoPlayer {
    /// Paused Next ring cap (step only — Play uses [`PlayIter`]).
    pub(super) fn play_presentation_cap(&self) -> usize {
        self.shared
            .track
            .metadata_ref()
            .play_policy()
            .presentation_cap
    }

    pub(super) fn apply_play_phase(&self, event: PlayPhaseEvent) {
        let next = play_phase_reduce(self.shared.play_phase.get(), event);
        self.shared.play_phase.set(next);
    }

    /// PlayPrime finished (clock armed or deferred silent) — render may present.
    ///
    /// `mediabunny: examples/media-player` `play` after `startVideoIterator`.
    pub fn begin_play_session(self: &Rc<Self>, pending_silent_clock: bool) {
        self.apply_play_phase(PlayPhaseEvent::Begin);
        self.shared.pending_silent_clock.set(pending_silent_clock);
        self.shared.resuming_clock.set(false);
        self.shared.held_media_origin.set(None);
        *self.shared.play_driver_error.borrow_mut() = None;
        // Scrub `then_play` prewarm (and any warm clock) can sit ahead of the
        // fresh PlayIter — suppress underrun→audio hold while video skip-catches
        // (same grace as tab-visible return).
        self.arm_underrun_catchup_grace();
        self.kick_update_next_frame();
    }

    /// True after [`Self::begin_play_session`] until pause / soft-hold / teardown.
    pub fn play_session_live(&self) -> bool {
        self.shared.play_phase.get().is_live()
    }

    pub(super) fn clear_play_session(&self) {
        self.apply_play_phase(PlayPhaseEvent::End);
        self.shared.pending_silent_clock.set(false);
        self.shared.resuming_clock.set(false);
        self.shared.held_media_origin.set(None);
        *self.shared.play_driver_error.borrow_mut() = None;
        self.end_play_iter();
    }

    /// mediabunny `videoFrameIterator.return()` — decoder back to the slot.
    pub(super) fn end_play_iter(&self) {
        self.shared.play_next.borrow_mut().take();
        let iter = self.shared.play_iter.borrow_mut().take();
        if let Some(decoder) = iter.and_then(|it| it.finish()) {
            self.put_decoder(decoder);
        }
    }

    fn is_current_iter(&self, iter: &PlayIter) -> bool {
        self.shared
            .play_iter
            .borrow()
            .as_ref()
            .is_some_and(|cur| cur.same(iter))
    }

    /// `nextFrame` waits, or the iterator can still yield samples.
    fn play_pending(&self) -> bool {
        if self.shared.play_next.borrow().is_some() {
            return true;
        }
        self.shared
            .play_iter
            .borrow()
            .as_ref()
            .is_some_and(|it| !it.exhausted())
    }

    /// Iterator stats for the pipeline meter (`None` when no Play range).
    pub(super) fn play_iter_stats(&self) -> Option<(usize, u32)> {
        self.shared
            .play_iter
            .borrow()
            .as_ref()
            .map(|it| (it.held_len(), it.decode_queue_size()))
    }

    /// One play rAF: silent-clock arm, render, underrun hold/resume, loop seam.
    ///
    /// `mediabunny: examples/media-player/media-player.ts` `render` + deferred
    /// `audioContextStartTime` on no-AAC (`finish_silent_play_prime`).
    pub fn play_raf_tick(self: &Rc<Self>) -> Result<PlayTick, MediaError> {
        if let Some(msg) = self.shared.play_driver_error.borrow_mut().take() {
            return Ok(PlayTick::Failed(msg));
        }
        if !self.play_session_live() || !self.is_playing() {
            return Ok(PlayTick::Skipped);
        }
        if !self.shared.play_phase.get().presents() || self.is_restarting() {
            return Ok(PlayTick::Skipped);
        }

        if self.shared.pending_silent_clock.get() {
            self.shared.pending_silent_clock.set(false);
            self.finish_silent_play_prime();
        }

        if let Some(iter) = self.shared.play_iter.borrow().as_ref() {
            iter.note_raf();
        }
        // Do **not** clear catch-up grace when `play_next` is Some — present may
        // take it this tick and leave `play_next` empty → underrun hold → audio gap.
        let t = self.media_time();
        let tick = match self.present_due(t)? {
            DuePresent::Ended => PlayTick::Ended,
            DuePresent::NeedsLoop => {
                self.start_loop_seam();
                PlayTick::Skipped
            }
            DuePresent::Playing { did_draw } => {
                self.tick_underrun_hold_resume();
                PlayTick::Presented { did_draw }
            }
        };
        self.kick_update_next_frame();
        Ok(tick)
    }

    /// mediabunny `startVideoIterator`: return the prior range, start at
    /// `start`, draw the first sample, keep the second as `nextFrame`.
    pub(super) async fn start_play_iter(
        &self,
        start: FrameId,
        epoch: u64,
    ) -> Result<(), MediaError> {
        self.end_play_iter();
        // Displayed / step VFs are stale once Play owns the canvas — free the pool.
        self.shared.displayed.borrow_mut().take();
        self.clear_presentation();
        let decoder = self.take_decoder(epoch).await?;
        let lead = self
            .shared
            .track
            .metadata_ref()
            .settle_policy()
            .play_index_lead;
        let iter = PlayIter::start(decoder, self.shared.track.clone(), start, lead);
        *self.shared.play_iter.borrow_mut() = Some(iter.clone());

        let first = iter.next().await?;
        if !self.is_current(epoch) || !self.is_current_iter(&iter) {
            return Err(MediaError::Cancelled);
        }
        if let Some((id, video)) = first {
            draw_frame_with_context(&self.shared.canvas, &self.shared.ctx, video.as_web_sys())?;
            drop(video);
            self.shared.commit_frame(id)?;
            self.touch_playhead(id);
        }
        let second = iter.next().await?;
        if !self.is_current(epoch) || !self.is_current_iter(&iter) {
            return Err(MediaError::Cancelled);
        }
        if let Some((id, video)) = second {
            let ts = self.frame_timestamp_seconds(id)?;
            let slot = self.copy_to_play_pool(id, video.as_web_sys())?;
            drop(video);
            *self.shared.play_next.borrow_mut() = Some(PlayNext {
                frame: id,
                ts,
                slot,
            });
        }
        Ok(())
    }

    fn copy_to_play_pool(
        &self,
        frame: FrameId,
        video: &web_sys::VideoFrame,
    ) -> Result<usize, MediaError> {
        let w = self.shared.canvas.width().max(1);
        let h = self.shared.canvas.height().max(1);
        let mut slot = self.shared.play_pool.borrow_mut();
        if slot.is_none() {
            *slot = Some(PlayCanvasPool::new()?);
        }
        slot.as_mut()
            .expect("pool created")
            .copy(frame, video, w, h)
    }

    /// Repaint the stage from the pool after a buffer resize (Play only).
    pub(super) fn repaint_from_play_pool(&self, frame: FrameId) -> Result<bool, MediaError> {
        let pool = self.shared.play_pool.borrow();
        let Some(pool) = pool.as_ref() else {
            return Ok(false);
        };
        let Some(slot) = pool.slot_for(frame) else {
            return Ok(false);
        };
        pool.draw(slot, &self.shared.canvas, &self.shared.ctx)?;
        Ok(true)
    }

    /// Spawn `updateNextFrame` unless one runs or `nextFrame` already waits.
    fn kick_update_next_frame(self: &Rc<Self>) {
        if self.shared.play_updating.get() || self.shared.play_next.borrow().is_some() {
            return;
        }
        let Some(iter) = self.shared.play_iter.borrow().clone() else {
            return;
        };
        if iter.exhausted() {
            return;
        }
        self.shared.play_updating.set(true);
        let player = Rc::clone(self);
        wasm_bindgen_futures::spawn_local(async move {
            let result = player.update_next_frame(&iter).await;
            player.shared.play_updating.set(false);
            if let Err(e) = result
                && !e.is_cancelled()
                && player.is_current_iter(&iter)
            {
                *player.shared.play_driver_error.borrow_mut() = Some(e.to_string());
            }
        });
    }

    /// mediabunny `updateNextFrame`: draw overdue samples immediately; keep the
    /// first future one as `nextFrame` (pool canvas; `VideoFrame` closed).
    async fn update_next_frame(&self, iter: &PlayIter) -> Result<(), MediaError> {
        let mut overdue = 0u64;
        loop {
            let item = iter.next().await?;
            if !self.is_current_iter(iter) || !self.is_playing() {
                return Ok(());
            }
            let Some((id, video)) = item else {
                break;
            };
            let ts = self.frame_timestamp_seconds(id)?;
            let t = self.media_time();
            if ts <= t && self.shared.play_phase.get() == PlayPhase::Live {
                overdue += 1;
                self.shared
                    .last_behind_ms
                    .set(Some(((t - ts) * 1000.0).max(0.0)));
                draw_frame_with_context(&self.shared.canvas, &self.shared.ctx, video.as_web_sys())?;
                drop(video);
                self.shared.commit_frame(id)?;
                self.touch_playhead(id);
                continue;
            }
            let slot = self.copy_to_play_pool(id, video.as_web_sys())?;
            drop(video);
            *self.shared.play_next.borrow_mut() = Some(PlayNext {
                frame: id,
                ts,
                slot,
            });
            break;
        }
        if overdue > 1 {
            self.shared
                .frames_skipped
                .set(self.shared.frames_skipped.get().saturating_add(overdue - 1));
            // Silent PlaybackClock only: realign to the painted playhead so the
            // next rAF is not still overdue (ChG skip storm). AAC stays master.
            if self
                .shared
                .playback_clock
                .as_ref()
                .is_some_and(|c| c.is_running())
            {
                self.snap_playing_clock_to_playhead();
            }
        }
        Ok(())
    }

    /// EOF while looping: restart the iterator at 0 under a soft-held clock.
    /// mediabunny: no loop in the example — `startVideoIterator(0)` + `play`.
    fn start_loop_seam(self: &Rc<Self>) {
        if self.shared.play_phase.get() != PlayPhase::Live {
            return;
        }
        self.apply_play_phase(PlayPhaseEvent::LoopStart);
        // If already HoldUnderrun, HoldScrub would FinishUnderrunTimer and
        // pollute holds — abandon first.
        self.apply_clock_event(crate::ClockEvent::ResetForPlay);
        let t = self.hold_media_clock_for_scrub();
        self.shared.held_media_origin.set(Some(t));
        let player = Rc::clone(self);
        wasm_bindgen_futures::spawn_local(async move {
            let epoch = player.shared.op_epoch.get();
            let result = player.start_play_iter(0, epoch).await;
            if player.shared.play_phase.get() != PlayPhase::LoopSeam {
                return;
            }
            player.apply_play_phase(PlayPhaseEvent::LoopDone);
            match result {
                Ok(()) => {
                    let t0 = player.state().timestamp.seconds;
                    if let Err(e) = player.resume_media_clock(t0).await
                        && !e.is_cancelled()
                    {
                        *player.shared.play_driver_error.borrow_mut() = Some(e.to_string());
                    }
                    player.shared.held_media_origin.set(None);
                }
                Err(e) if e.is_cancelled() => {}
                Err(e) => {
                    *player.shared.play_driver_error.borrow_mut() = Some(e.to_string());
                }
            }
        });
    }

    /// Tab hidden or post-visible grace — skip video-underrun → audio silence.
    pub(super) fn suppress_underrun_for_video_catchup(&self) -> bool {
        if self.shared.scrub_bg_suspended() {
            return true;
        }
        let Some(until) = self.shared.underrun_catchup_until_ms.get() else {
            return false;
        };
        let now = now_ms().unwrap_or(0.0);
        if now >= until {
            self.shared.underrun_catchup_until_ms.set(None);
            return false;
        }
        true
    }

    /// After tab visible while Playing — allow video skip-catch without audio hold.
    pub(super) fn arm_underrun_catchup_grace(&self) {
        // Long enough for rAF/iterator to repark after a hidden-tab lag; time-only
        // (not cleared on first `play_next` — that raced hold in the same tick).
        const GRACE_MS: f64 = 1000.0;
        if let Some(now) = now_ms() {
            self.shared
                .underrun_catchup_until_ms
                .set(Some(now + GRACE_MS));
        }
    }

    fn tick_underrun_hold_resume(self: &Rc<Self>) {
        // Tab hidden / catch-up grace: video `nextFrame` lags — never silence audio.
        // Leave any in-flight HoldUnderrun even without `nextFrame`.
        // `mediabunny: media-player` — no video-underrun → audio hold.
        if self.suppress_underrun_for_video_catchup() {
            if self.shared.clock_state.get().underrun_holding() && !self.shared.resuming_clock.get()
            {
                self.spawn_resume_after_underrun_hold();
            }
            return;
        }

        if self.play_underrun_hold() {
            let cs = self.shared.clock_state.get();
            if cs.underrun_holding() || self.shared.resuming_clock.get() {
                return;
            }
            let t = self.hold_media_clock_for_underrun();
            self.shared.held_media_origin.set(Some(t));
            return;
        }

        let should_resume = self.shared.clock_state.get().underrun_holding()
            && !self.shared.resuming_clock.get()
            && self.shared.play_next.borrow().is_some();
        if !should_resume {
            return;
        }
        self.spawn_resume_after_underrun_hold();
    }

    pub(super) fn spawn_resume_after_underrun_hold(self: &Rc<Self>) {
        self.shared.resuming_clock.set(true);
        let player = Rc::clone(self);
        wasm_bindgen_futures::spawn_local(async move {
            let t = player
                .shared
                .held_media_origin
                .get()
                .unwrap_or_else(|| player.state().timestamp.seconds);
            let result = player.resume_media_clock(t).await;
            player.shared.resuming_clock.set(false);
            match result {
                Ok(()) => player.shared.held_media_origin.set(None),
                Err(e) if e.is_cancelled() => {}
                Err(e) => {
                    *player.shared.play_driver_error.borrow_mut() = Some(e.to_string());
                }
            }
        });
    }

    /// Shrink Exact to leave GPU headroom for Play VF pool; keep near-playhead bitmaps.
    pub(super) fn trim_exact_for_play(&self) {
        let Ok(mut blit) = self.shared.scrub_blit.try_borrow_mut() else {
            return;
        };
        let meta = self.shared.track.metadata();
        let policy = meta.scrub_policy();
        let buf_w = self.shared.canvas.width().max(1);
        let buf_h = self.shared.canvas.height().max(1);
        let full = scrub_blit_frame_budget(buf_w, buf_h, policy.blit_mib);
        let play_vfs = 1 + meta.play_policy().presentation_cap + PLAY_EXACT_VRAM_VF_SLACK;
        let reserve =
            exact_frames_to_free_for_play_vram(play_vfs, meta.width, meta.height, buf_w, buf_h);
        blit.set_playhead(self.state().frame);
        blit.set_max(scrub_blit_cap_during_play(full, reserve));
    }

    /// PlayPrime: abort densify, warm index + bytes, then `startVideoIterator`.
    pub async fn prime_playback(&self) -> Result<(), MediaError> {
        if !self.is_playing() {
            return Ok(());
        }
        // `play()` aborts densify without awaiting. Soft-cold settle leaves
        // `scrub_busy` true → fill hangs on long-GOP URL.
        // mediabunny: examples/media-player seekToTime →
        // `await videoFrameIterator?.return()` before play / startVideoIterator.
        self.abort_scrub_fill(false);
        self.await_scrub_densify_yield(false).await;
        if !self.is_playing() {
            return Ok(());
        }
        let epoch = self.shared.op_epoch.get();
        let settle = self.shared.track.metadata_ref().settle_policy();
        // fMP4: arm a long index lead in bg, but only *await* one urgent batch
        // past the current index (`mediabunny: first samples before play`).
        let play_index_lead = settle.play_index_lead;
        let play_index_await_batch = settle.play_index_await_batch;
        let head = self.state().frame;
        {
            let track = self.shared.track.clone();
            if !track.fragments_complete() {
                let want = head.saturating_add(play_index_lead);
                track.request_index_through(want);
                let indexed = track.indexed_frame_count();
                let await_through = want.min(indexed.saturating_add(play_index_await_batch));
                if await_through > indexed {
                    track.ensure_indexed_through(await_through).await?;
                    if !self.is_current(epoch) || !self.is_playing() {
                        return Err(MediaError::Cancelled);
                    }
                }
            }
        }
        // Await key→head (+ a little ahead) bytes so the first samples are not
        // stuck behind cold Ranges; the rest prefetches in the background.
        let warm_t0 = now_ms().unwrap_or(0.0);
        let (warm_secs, warm_key) = {
            let track = self.shared.track.clone();
            let key = track.index().nearest_keyframe(head).unwrap_or(head);
            let gop_lead = super::scrub_gap::gop_sample_lead(&track, key).min(play_index_lead);
            let await_span = head
                .saturating_sub(key)
                .saturating_add(gop_lead.min(8))
                .max(gop_lead)
                .max(1);
            track.warm_sample_bytes(key, await_span).await?;
            track.prefetch_samples_ahead(head, play_index_lead);
            let last = head
                .saturating_add(gop_lead.min(8))
                .min(track.indexed_frame_count().saturating_sub(1));
            let secs = track
                .index()
                .get(last)
                .map(|e| e.composition_timestamp().seconds)
                .unwrap_or(0.0);
            let warm_ms = now_ms().unwrap_or(warm_t0) - warm_t0;
            web_sys::console::log_1(
                &format!("fpmedia-web: play prime warm key={key} span={await_span} {warm_ms:.0}ms")
                    .into(),
            );
            (secs, key)
        };
        if !self.is_current(epoch) || !self.is_playing() {
            return Err(MediaError::Cancelled);
        }
        // Audio bulk warm is 0..t — only near start (mid-clip pump primes at origin).
        if warm_secs > 0.0 && warm_key == 0 {
            let audio = self.shared.audio.borrow().as_ref().map(Rc::clone);
            if let Some(a) = audio {
                a.warm_bytes_through(warm_secs).await?;
                if !self.is_current(epoch) || !self.is_playing() {
                    return Err(MediaError::Cancelled);
                }
            }
        }
        match self.start_play_iter(head, epoch).await {
            Ok(()) => Ok(()),
            Err(e) if e.is_cancelled() => Err(e),
            // Poison after Exact/densify race — one fresh range.
            Err(e) if e.is_decoder_poison() && self.is_current(epoch) && self.is_playing() => {
                web_sys::console::warn_1(
                    &format!("fpmedia-web: PlayPrime iterator poison → restart ({e})").into(),
                );
                self.start_play_iter(head, epoch).await
            }
            Err(e) => Err(e),
        }
    }

    /// Paused Next ring length (0 while playing).
    pub fn presentation_len(&self) -> usize {
        self.shared.presentation.borrow().len()
    }

    /// True underrun: no `nextFrame` while the next timeline frame is already due.
    ///
    /// Used to freeze the media clock. Distinct from a healthy wait for the next
    /// frame's deadline. Tab-hidden / post-visible catch-up: always false — video
    /// lag must not [`crate::AudioOutput::hold_underrun`] (choppy / micro-pause audio).
    pub fn play_underrun_hold(&self) -> bool {
        if !self.is_playing()
            || self.shared.session.get().is_restarting()
            || !self.shared.play_phase.get().presents()
            || self.suppress_underrun_for_video_catchup()
        {
            return false;
        }
        // Shuttle: keep clock running (overdue frames are drawn as they arrive).
        if self.is_shuttle() {
            return false;
        }
        if self.shared.play_next.borrow().is_some() || !self.play_pending() {
            return false;
        }
        let state = self.state();
        let next = state.frame.saturating_add(1);
        if next >= state.frame_count {
            return false;
        }
        let Ok(start) = self.frame_timestamp_seconds(next) else {
            return false;
        };
        self.media_time() >= start
    }

    /// Snap live play clock back to the committed playhead (overdue storm).
    ///
    /// AAC: soft-hold then `restart_warm`. Silent [`PlaybackClock`] / wall: reset
    /// origins.
    pub(super) fn snap_playing_clock_to_playhead(&self) {
        let playhead = self.state().frame;
        let Ok(t) = self.frame_timestamp_seconds(playhead) else {
            return;
        };
        let audio = self.shared.audio.borrow().as_ref().map(Rc::clone);
        if let Some(a) = audio.filter(|a| a.is_playing()) {
            let _ = a.hold_underrun();
            self.shared.clock.borrow_mut().start(t);
            self.shared.wall_origin.set(None);
            let shared = Rc::clone(&self.shared);
            wasm_bindgen_futures::spawn_local(async move {
                if shared.is_disposed() || !shared.playing.get() {
                    return;
                }
                let audio = shared.audio.borrow().as_ref().map(Rc::clone);
                if let Some(a) = audio
                    && let Err(e) = a.restart_warm(t).await
                {
                    web_sys::console::warn_1(
                        &format!("fpmedia-web: clock snap restart_warm: {e}").into(),
                    );
                }
            });
        } else if let Some(c) = self.shared.playback_clock.as_ref() {
            self.shared.clock.borrow_mut().start(t);
            self.shared.wall_origin.set(None);
            c.start(t);
        } else {
            self.shared.clock.borrow_mut().start(t);
            if let Some(now) = now_ms() {
                self.shared.wall_origin.set(Some(now / 1000.0));
            }
        }
    }

    /// mediabunny `render`: draw `nextFrame` once its timestamp is due.
    pub fn present_due(&self, media_time_secs: f64) -> Result<DuePresent, MediaError> {
        if !self.is_playing() {
            return Ok(DuePresent::Ended);
        }

        let count = self.state().frame_count;
        if count == 0 {
            self.shared.playing.set(false);
            return Ok(DuePresent::Ended);
        }

        let last_end = {
            let track = &self.shared.track;
            let index = track.index();
            let indexed = index.len() as u64;
            if indexed == 0 {
                return Ok(DuePresent::Ended);
            }
            // fMP4 warm-open: `frame_count` is provisional (tfra) while the index is
            // still short — never `get(count-1)` until backfill finishes.
            let last = index.get(indexed - 1).ok_or(MediaError::FrameOutOfRange(
                indexed.saturating_sub(1),
                count,
            ))?;
            let sample_end = last.composition_timestamp().seconds
                + (last.duration_ticks as f64 / last.timescale.max(1) as f64);
            eof_timeline_end(
                sample_end,
                track.metadata_ref().duration().seconds,
                indexed,
                count,
            )
        };
        if let Some(wall) = eof_wall_decision(
            media_time_secs >= last_end,
            self.play_pending(),
            self.is_looping(),
        ) {
            match wall {
                EofWall::DrainRemaining => {}
                EofWall::NeedsLoop => return Ok(DuePresent::NeedsLoop),
                EofWall::Ended => {
                    self.shared.playing.set(false);
                    self.end_play_iter();
                    return Ok(DuePresent::Ended);
                }
            }
        }

        let due = self
            .shared
            .play_next
            .borrow()
            .as_ref()
            .is_some_and(|n| n.ts <= media_time_secs);
        if !due {
            return Ok(DuePresent::Playing { did_draw: false });
        }
        let next = self
            .shared
            .play_next
            .borrow_mut()
            .take()
            .expect("due checked");
        if let Some(pool) = self.shared.play_pool.borrow().as_ref() {
            pool.draw(next.slot, &self.shared.canvas, &self.shared.ctx)?;
        }
        let lag_ms = ((media_time_secs - next.ts) * 1000.0).max(0.0);
        if lag_ms > 1.0 {
            self.shared.last_behind_ms.set(Some(lag_ms));
        }
        self.shared.commit_frame(next.frame)?;
        self.touch_playhead(next.frame);
        Ok(DuePresent::Playing { did_draw: true })
    }
}
