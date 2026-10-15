//! Play / present / prefetch / loop orchestration for [`super::VideoPlayer`].

use std::collections::VecDeque;
use std::rc::Rc;

use crate::error::MediaError;
use crate::policy::playback::{
    EofWall, eof_timeline_end, eof_wall_decision, loop_drain_frames, near_eof_by_frames,
};
use crate::policy::{
    exact_frames_to_free_for_play_vram, scrub_blit_cap_during_play, scrub_blit_frame_budget,
};
use crate::types::FrameId;

use super::{
    BIG_LAG_SECONDS, DuePresent, LOOP_NEAR_END_PRESENTATION_CAP_MAX, PLAY_EXACT_VRAM_VF_SLACK,
    PREFETCH_DEADLINE_SLACK_MS, PlayTick, QueuedFrame, SHUTTLE_PRESENTATION_QUEUE_CAP, VideoPlayer,
    next_animation_frame, now_ms, sleep_ms,
};

impl VideoPlayer {
    pub(super) fn play_presentation_cap(&self) -> usize {
        if self.is_shuttle() {
            return SHUTTLE_PRESENTATION_QUEUE_CAP;
        }
        let policy = self.shared.track.metadata_ref().play_policy();
        // Loop: keep a long drain buffer while `fill_loop_prime` owns the decoder.
        if self.is_looping() && self.is_playing() && self.near_loop_prepare_zone() {
            return self.loop_drain_frames_for_rate();
        }
        // Uhd (loop or not): once inside eof lead, allow the whole remaining
        // tail into the presentation queue so last-GOP decode does not underrun.
        // Hd `eof_lead_secs == 0` → never enters (FHD unchanged).
        if policy.eof_lead_secs() > 0.0 && self.near_eof_lead(policy.eof_lead_secs()) {
            let remaining = self.frames_left_on_timeline();
            return remaining
                .max(policy.presentation_cap)
                .min(LOOP_NEAR_END_PRESENTATION_CAP_MAX);
        }
        policy.presentation_cap
    }

    /// Frames from current playhead through EOF (inclusive of current).
    pub(super) fn frames_left_on_timeline(&self) -> usize {
        let state = self.state();
        state.frame_count.saturating_sub(state.frame).max(1) as usize
    }

    /// True when playhead is within `lead_secs` of EOF (rate-scaled).
    pub(super) fn near_eof_lead(&self, lead_secs: f64) -> bool {
        let count = self.state().frame_count;
        if count == 0 {
            return false;
        }
        let fps = self
            .shared
            .track
            .metadata_ref()
            .fps
            .filter(|f| f.is_finite() && *f > 0.0)
            .unwrap_or(24.0);
        near_eof_by_frames(
            self.frames_left_on_timeline() as f64,
            lead_secs,
            fps,
            self.playback_rate(),
        )
    }

    /// Frames to keep queued near EOF so `fill_loop_prime` finishes before drain.
    pub(super) fn loop_drain_frames_for_rate(&self) -> usize {
        let policy = self.shared.track.metadata_ref().play_policy();
        let fps = self
            .shared
            .track
            .metadata_ref()
            .fps
            .filter(|f| f.is_finite() && *f > 0.0)
            .unwrap_or(24.0);
        let floor = policy.presentation_cap.max(policy.loop_prime_frames());
        loop_drain_frames(
            policy.loop_prime_budget_secs(),
            fps,
            self.playback_rate(),
            floor,
            LOOP_NEAR_END_PRESENTATION_CAP_MAX,
        )
    }

    /// Playhead is within the rate-scaled loop-prepare lead of EOF.
    pub(super) fn near_loop_prepare_zone(&self) -> bool {
        let count = self.state().frame_count;
        if count == 0 {
            return false;
        }
        let lead = self.loop_drain_frames_for_rate() as FrameId;
        // Playhead only — must not borrow `presentation` (callers may hold it).
        count.saturating_sub(self.state().frame) <= lead
    }

    /// True when the play prefetch loop should decode another ahead/behind frame.
    pub fn wants_play_prefetch(&self) -> bool {
        self.is_playing()
            && self.shared.play_session_live.get()
            && !self.shared.play_loop_seam.get()
            && !self.shared.resuming_clock.get()
            && !self.shared.session.get().blocks_prefetch()
            && self.presentation_len() < self.play_presentation_cap()
    }

    /// PlayPrime finished (clock armed or deferred silent) — host may present.
    ///
    /// Starts the prefetch / loop-prepare driver. `mediabunny: examples/media-player`
    /// `play` after `startVideoIterator` while `render` already loops.
    pub fn begin_play_session(self: &Rc<Self>, pending_silent_clock: bool) {
        self.shared.play_session_live.set(true);
        self.shared.pending_silent_clock.set(pending_silent_clock);
        self.shared.resuming_clock.set(false);
        self.shared.play_loop_seam.set(false);
        self.shared.held_media_origin.set(None);
        *self.shared.play_driver_error.borrow_mut() = None;
        let driver = self.shared.play_prefetch_gen.get().wrapping_add(1);
        self.shared.play_prefetch_gen.set(driver);
        self.spawn_play_prefetch_loop(driver);
    }

    /// True after [`Self::begin_play_session`] until pause / soft-hold / teardown.
    pub fn play_session_live(&self) -> bool {
        self.shared.play_session_live.get()
    }

    pub(super) fn clear_play_session(&self) {
        self.shared.play_session_live.set(false);
        self.shared.pending_silent_clock.set(false);
        self.shared.resuming_clock.set(false);
        self.shared.play_loop_seam.set(false);
        self.shared.held_media_origin.set(None);
        *self.shared.play_driver_error.borrow_mut() = None;
        // Invalidate in-flight prefetch drivers.
        let driver = self.shared.play_prefetch_gen.get().wrapping_add(1);
        self.shared.play_prefetch_gen.set(driver);
    }

    fn spawn_play_prefetch_loop(self: &Rc<Self>, driver: u64) {
        let player = Rc::clone(self);
        wasm_bindgen_futures::spawn_local(async move {
            while player.shared.play_prefetch_gen.get() == driver
                && player.is_playing()
                && player.play_session_live()
            {
                if player.wants_play_prefetch() {
                    match player.prefetch_one().await {
                        Ok(true) => continue,
                        Ok(false) => {}
                        Err(e) if e.is_cancelled() => break,
                        Err(e) => {
                            if player.shared.play_prefetch_gen.get() == driver {
                                *player.shared.play_driver_error.borrow_mut() = Some(e.to_string());
                                player.pause();
                            }
                            break;
                        }
                    }
                }
                if player.is_looping() {
                    match player.prepare_loop_if_needed().await {
                        Ok(()) => {}
                        Err(e) if e.is_cancelled() => break,
                        Err(e) => {
                            if player.shared.play_prefetch_gen.get() == driver {
                                *player.shared.play_driver_error.borrow_mut() = Some(e.to_string());
                                player.pause();
                            }
                            break;
                        }
                    }
                }
                next_animation_frame().await;
            }
        });
    }

    /// One play rAF: silent-clock arm, present, underrun hold/resume, loop seam.
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
        if self.shared.play_loop_seam.get() || self.is_restarting() {
            return Ok(PlayTick::Skipped);
        }

        if self.shared.pending_silent_clock.get() {
            self.shared.pending_silent_clock.set(false);
            let media_origin = self
                .shared
                .held_media_origin
                .get()
                .unwrap_or_else(|| self.state().timestamp.seconds);
            match self.finish_silent_play_prime() {
                Ok(_) => {}
                Err(e) => {
                    web_sys::console::warn_1(
                        &format!("fpmedia-web: silent play prime present: {e}").into(),
                    );
                    self.arm_silent_play_clock(media_origin);
                }
            }
        }

        if self.is_hidden_hold() {
            return Ok(PlayTick::Skipped);
        }

        let t = self.media_time();
        match self.present_due(t)? {
            DuePresent::Ended => Ok(PlayTick::Ended),
            DuePresent::NeedsLoop => {
                self.start_loop_seam();
                Ok(PlayTick::Skipped)
            }
            DuePresent::Playing { did_draw } => {
                self.tick_underrun_hold_resume();
                Ok(PlayTick::Presented { did_draw })
            }
        }
    }

    fn start_loop_seam(self: &Rc<Self>) {
        if self.shared.play_loop_seam.get() {
            return;
        }
        self.shared.play_loop_seam.set(true);
        // Freeze the timeline while loop owns the decoder — leaving the clock
        // running through `loop_to_start` skips/underruns on resume (~1 s hold
        // on GOP12 loop probes). Soft-hold via HoldScrub (not HoldUnderrun).
        // If already HoldUnderrun, HoldScrub would FinishUnderrunTimer and
        // pollute holdsB — abandon first. mediabunny: no loop; local adaptation.
        self.apply_clock_event(crate::ClockEvent::ResetForPlay);
        let t = self.hold_media_clock_for_scrub();
        self.shared.held_media_origin.set(Some(t));
        let player = Rc::clone(self);
        wasm_bindgen_futures::spawn_local(async move {
            match player.loop_to_start().await {
                Ok(()) => {
                    // Clock still soft-held — seed ahead frames before resume so
                    // the first post-wrap present does not re-enter HoldUnderrun.
                    player.shared.play_loop_seam.set(false);
                    for _ in 0..8 {
                        if !player.wants_play_prefetch() {
                            break;
                        }
                        match player.prefetch_one().await {
                            Ok(true) => {}
                            _ => break,
                        }
                    }
                    let _ = player.resume_media_clock(0.0).await;
                    player.shared.held_media_origin.set(None);
                }
                Err(e) if e.is_cancelled() => {
                    player.shared.play_loop_seam.set(false);
                }
                Err(e) => {
                    *player.shared.play_driver_error.borrow_mut() = Some(e.to_string());
                    player.shared.play_loop_seam.set(false);
                    player.pause();
                }
            }
        });
    }

    fn tick_underrun_hold_resume(self: &Rc<Self>) {
        if self.play_underrun_hold() {
            let cs = self.shared.clock_state.get();
            if cs.hidden_holding()
                || cs.underrun_holding()
                || self.shared.resuming_clock.get()
                || self.shared.play_loop_seam.get()
                || self.is_restarting()
            {
                return;
            }
            let t = self.hold_media_clock_for_underrun();
            self.shared.held_media_origin.set(Some(t));
            return;
        }

        let should_resume = self.shared.clock_state.get().underrun_holding()
            && !self.shared.resuming_clock.get()
            && !self.shared.play_loop_seam.get()
            && !self.is_restarting()
            && self.presentation_len() > 0;
        if !should_resume {
            return;
        }
        self.shared.resuming_clock.set(true);
        let player = Rc::clone(self);
        wasm_bindgen_futures::spawn_local(async move {
            let t = player
                .shared
                .held_media_origin
                .get()
                .unwrap_or_else(|| player.state().timestamp.seconds);
            match player.resume_media_clock(t).await {
                Ok(()) => {
                    player.shared.held_media_origin.set(None);
                    player.shared.resuming_clock.set(false);
                }
                Err(e) if e.is_cancelled() => {
                    player.shared.resuming_clock.set(false);
                }
                Err(e) => {
                    player.shared.resuming_clock.set(false);
                    *player.shared.play_driver_error.borrow_mut() = Some(e.to_string());
                    player.pause();
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

    pub(super) async fn step_playback(&self) -> Result<bool, MediaError> {
        if !self.is_playing() {
            return Ok(false);
        }
        let next = self.state().frame.saturating_add(1);
        if next >= self.state().frame_count {
            return self.end_or_loop().await;
        }
        self.present_frame(next).await?;
        Ok(self.has_more_playback())
    }

    pub async fn skip_behind(&self, behind_seconds: f64) -> Result<bool, MediaError> {
        if !self.is_playing() || behind_seconds <= 0.0 {
            return Ok(self.has_more_playback());
        }
        let next = self.state().frame.saturating_add(1);
        if next >= self.state().frame_count {
            return self.end_or_loop().await;
        }

        let target = self.catch_up_frame(next, behind_seconds)?;
        if target <= next {
            return self.step_playback().await;
        }

        self.shared.frames_skipped.set(
            self.shared
                .frames_skipped
                .get()
                .saturating_add(target - next),
        );
        self.shared
            .last_behind_ms
            .set(Some(behind_seconds * 1000.0));

        self.drain_presentation_before(target);
        {
            let mut presentation = self.shared.presentation.borrow_mut();
            if presentation.front().is_some_and(|f| f.frame == target) {
                let queued = presentation.pop_front().expect("front checked");
                drop(presentation);
                // Play path: present + Exact ingest (same as present_due).
                self.present_play_frame(queued.frame, queued.video)?;
                self.touch_playhead(queued.frame);
                self.shared.commit_frame(queued.frame)?;
                return Ok(self.has_more_playback());
            }
        }

        self.clear_presentation();
        // Soft catch-up (mediabunny `updateNextFrame` loop): keep warm decode +
        // dense when the target stays in the same GOP. Hard invalidate only when
        // catch-up crosses into a later GOP — a full key→N after wipe was Chrome
        // long-GOP's stutter loop (play a few → clear strip → flush/idle mid-GOP).
        // Do **not** `clear_scrub_store` here — scrub residency is unrelated to
        // underrun and wiping it was the visible "cache vanishes" symptom.
        // Behind ring is stale past the catch-up target — drop it.
        self.clear_step_ring();
        let hard_seek = !self.is_all_intra() && {
            let index = self.shared.track.index();
            match (index.nearest_keyframe(next), index.nearest_keyframe(target)) {
                (Some(nk), Some(tk)) => tk != nk,
                _ => true,
            }
        };
        if hard_seek {
            self.invalidate_decoder();
        }
        let epoch = self.shared.op_epoch.get();
        self.goto_frame(target, epoch).await?;
        Ok(self.has_more_playback())
    }

    pub(super) async fn end_or_loop(&self) -> Result<bool, MediaError> {
        if !self.is_looping() {
            self.shared.playing.set(false);
            self.clear_presentation();
            self.clear_step_ring();
            return Ok(false);
        }
        self.clear_presentation();
        self.clear_step_ring();
        self.invalidate_decoder();
        let epoch = self.shared.op_epoch.get();
        self.goto_frame(0, epoch).await?;
        Ok(true)
    }

    pub(super) fn has_more_playback(&self) -> bool {
        if !self.is_playing() {
            return false;
        }
        self.is_looping() || self.state().frame + 1 < self.state().frame_count
    }

    pub async fn prime_playback(&self) -> Result<(), MediaError> {
        if !self.is_playing() {
            return Ok(());
        }
        // `play()` aborts densify without awaiting. Soft-cold settle leaves
        // `scrub_busy` true → fill hangs on long-GOP URL (stress 2997/60
        // gop33/gop48: Play disabled / ensurePlaying timeout).
        // mediabunny: examples/media-player seekToTime →
        // `await videoFrameIterator?.return()` before play / startVideoIterator.
        self.abort_scrub_fill(false);
        self.await_scrub_densify_yield(false).await;
        if !self.is_playing() {
            return Ok(());
        }
        let epoch = self.shared.op_epoch.get();
        // Exact settle seeds play_prime_frames (mediabunny `startVideoIterator`).
        // Soft Pause leaves Warm audio. Either way: skip blocking byte-warm and
        // do not re-decode into an already-primed presentation queue.
        // mediabunny: examples/media-player seekToTime / pause → play.
        let settle = self.shared.track.metadata_ref().settle_policy();
        let prime_target = settle.play_prime_frames.min(self.play_presentation_cap());
        let presentation_primed = self.presentation_len() >= prime_target;
        // Skip byte-warm only when Exact/settle already seeded the presentation
        // queue. Soft Pause alone must not skip: soft-cold settle leaves Warm
        // audio + empty queue mid-GOP → fill blocked on cold key Ranges.
        // mediabunny: first samples for seek `t` (key→target), not pause latch.
        let skip_byte_warm = presentation_primed;
        // fMP4: arm a long index lead in bg, but only *await* one urgent batch
        // past the current index so Play click / PlayPrime cannot stall for
        // many RTTs (`mediabunny: first samples before play` — not the whole
        // warm→lead gap on the click turn).
        let play_index_lead = settle.play_index_lead;
        let play_index_await_batch = settle.play_index_await_batch;
        // Unprimed cold PlayPrime: await one GOP from the keyframe (not from
        // head mid-GOP) so soft-cold→Play can cascade. Warm from `head` left
        // key samples cold → decode hung (stress 60/2997 long-GOP).
        // mediabunny: first samples for seek `t`, not a long timeline warm.
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
        if skip_byte_warm {
            web_sys::console::log_1(
                &format!(
                    "fpmedia-web: play prime skip byte warm (warm_audio={} primed={presentation_primed})",
                    self.audio_warm_resume()
                )
                .into(),
            );
            let track = self.shared.track.clone();
            track.prefetch_samples_ahead(head, play_index_lead);
        } else {
            let warm_t0 = now_ms().unwrap_or(0.0);
            let (warm_secs, warm_key) = {
                let track = self.shared.track.clone();
                let key = track.index().nearest_keyframe(head).unwrap_or(head);
                let gop_lead = super::scrub_gap::gop_sample_lead(&track, key).min(play_index_lead);
                // Cover key→head (cascade) + small play-ahead. Clamped gop_lead
                // alone (≤24) misses late frames in GOP48 mid-GOP soft-cold.
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
                    &format!(
                        "fpmedia-web: play prime warm key={key} span={await_span} {warm_ms:.0}ms"
                    )
                    .into(),
                );
                (secs, key)
            };
            if !self.is_current(epoch) || !self.is_playing() {
                return Err(MediaError::Cancelled);
            }
            // Audio bulk warm is 0..t (up to 8 MiB). Fine near start; mid-clip
            // soft-cold→Play stalled for seconds on URL Ranges. Pump primes at
            // origin instead (`start_spawned` / `restart_warm`).
            // mediabunny: first samples around playhead, not full prefix.
            if warm_secs > 0.0 && warm_key == 0 {
                let audio = self.shared.audio.borrow().as_ref().map(Rc::clone);
                if let Some(a) = audio {
                    a.warm_bytes_through(warm_secs).await?;
                    if !self.is_current(epoch) || !self.is_playing() {
                        return Err(MediaError::Cancelled);
                    }
                }
            }
        }
        if presentation_primed {
            web_sys::console::log_1(&"fpmedia-web: play prime skip fill (settle seeded)".into());
            return Ok(());
        }
        // Scrub store is markers + blit — play re-decodes into the presentation
        // queue. Do **not** trim scrub residency on Play — eviction is budget /
        // farthest-from-aim only (Pause densify paid for that trail).
        // Poison after Exact/densify race: soft-park retry alone cannot heal a
        // closed main — recreate once then refill (parity Exact retry).
        // mediabunny: seekToTime → startVideoIterator (return prior + new canvases).
        match self.fill_presentation_queue(epoch).await {
            Ok(()) => Ok(()),
            Err(e) if e.is_cancelled() => Err(e),
            Err(e) if e.is_decoder_poison() && self.is_current(epoch) && self.is_playing() => {
                web_sys::console::warn_1(
                    &format!("fpmedia-web: PlayPrime fill poison → recreate ({e})").into(),
                );
                self.clear_presentation();
                self.invalidate_decoder();
                self.fill_presentation_queue(epoch).await
            }
            Err(e) => Err(e),
        }
    }

    pub fn presentation_len(&self) -> usize {
        self.shared.presentation.borrow().len()
    }

    /// True underrun: presentation empty while the next frame is already due.
    ///
    /// Used to freeze the media clock (not for `skip_behind`). Distinct from a
    /// healthy empty queue while waiting for the next frame's deadline.
    pub fn play_underrun_hold(&self) -> bool {
        if !self.is_playing() || self.shared.session.get().is_restarting() {
            return false;
        }
        // Shuttle: keep clock running (skip overdue when frames arrive).
        if self.is_shuttle() {
            return false;
        }
        if self.presentation_len() > 0 {
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

    /// Media-clock seconds ahead of the next needed frame while the presentation
    /// queue is empty (play underrun). `None` if not underrunning or lag below
    /// [`BIG_LAG_SECONDS`]. Prefer [`Self::play_underrun_hold`] + clock freeze for
    /// Chrome; `skip_behind` remains for explicit catch-up.
    pub fn play_underrun_behind(&self) -> Option<f64> {
        if !self.play_underrun_hold() {
            return None;
        }
        let state = self.state();
        let next = state.frame.saturating_add(1);
        let start = self.frame_timestamp_seconds(next).ok()?;
        let behind = self.media_time() - start;
        (behind >= BIG_LAG_SECONDS).then_some(behind)
    }

    /// True when the presentation queue already holds through the last frame.
    pub(super) fn presentation_reaches_eof(&self) -> bool {
        let count = self.state().frame_count;
        if count == 0 {
            return false;
        }
        self.shared
            .presentation
            .borrow()
            .back()
            .is_some_and(|q| q.frame + 1 >= count)
    }

    /// Snap live play clock back to the committed playhead (shuttle clock race).
    ///
    /// AAC: soft-hold then `restart_warm`. Silent [`PlaybackClock`] / wall: reset
    /// origins. Audio-driven shuttle: soft-hold then `restart_warm` so
    /// `media_time()` follows again.
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

    /// Sync present — safe to call while [`Self::prefetch_one`] is awaiting.
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
            self.presentation_len(),
            self.state().frame,
            count.saturating_sub(1),
            self.is_looping(),
        ) {
            match wall {
                // Fall through to present remaining due frames at EOF.
                EofWall::DrainRemaining => {}
                // Shuttle / wall-clock: media_time raced past duration mid-timeline.
                // mediabunny: no equivalent (no rate API).
                EofWall::SnapClockContinue => {
                    self.snap_playing_clock_to_playhead();
                    return Ok(DuePresent::Playing { did_draw: false });
                }
                EofWall::NeedsLoop => return Ok(DuePresent::NeedsLoop),
                EofWall::Ended => {
                    self.shared.playing.set(false);
                    self.clear_presentation();
                    self.clear_step_ring();
                    return Ok(DuePresent::Ended);
                }
            }
        }

        let mut did_draw = false;
        let mut skipped_any = false;
        loop {
            let front_frame = self.shared.presentation.borrow().front().map(|q| q.frame);
            let Some(front_frame) = front_frame else {
                break;
            };
            let start = self.frame_timestamp_seconds(front_frame)?;
            if start > media_time_secs {
                break;
            }

            let next_id = self.shared.presentation.borrow().get(1).map(|n| n.frame);
            let next_also_due = match next_id {
                Some(n) => self.frame_timestamp_seconds(n)? <= media_time_secs,
                None => false,
            };

            let queued = self
                .shared
                .presentation
                .borrow_mut()
                .pop_front()
                .expect("front checked");
            // Drop overdue queued frames so Play stays on the clock (smoothness).
            // Product: Exact = seek/Pause; Play ≠ paint every frame (`product.md`).
            // mediabunny `updateNextFrame`: overdue samples are drawn in a tight
            // loop (only the last is visible) — equivalent to skip-to-latest here.
            // Do **not** soft-pace / catch-up burst (micro-stutter).
            if next_also_due {
                skipped_any = true;
                let lag_ms = ((media_time_secs - start) * 1000.0).max(0.0);
                self.shared.last_behind_ms.set(Some(lag_ms));
                self.shared
                    .frames_skipped
                    .set(self.shared.frames_skipped.get().saturating_add(1));
                drop(queued.video);
                self.shared.commit_frame(queued.frame)?;
                continue;
            }

            let lag_ms = ((media_time_secs - start) * 1000.0).max(0.0);
            if lag_ms > 1.0 {
                self.shared.last_behind_ms.set(Some(lag_ms));
            }
            // Play: Exact ingest of presented frame; densify stays off.
            self.present_play_frame(queued.frame, queued.video)?;
            self.touch_playhead(queued.frame);
            self.shared.commit_frame(queued.frame)?;
            did_draw = true;
            break;
        }

        // Silent PlaybackClock only: after skip-to-latest, realign the driver to
        // the painted playhead so the next rAF is not still overdue (ChG full
        // multi-second skip storm while decode catches up). AAC keeps audio as
        // master — do not snap. mediabunny: updateNextFrame draws overdue then
        // waits for a future sample; no separate skip meter.
        if skipped_any
            && did_draw
            && self
                .shared
                .playback_clock
                .as_ref()
                .is_some_and(|c| c.is_running())
        {
            self.snap_playing_clock_to_playhead();
        }

        Ok(DuePresent::Playing { did_draw })
    }

    /// Reverse present: key-hop only — show older sync, snap clock (`mediabunny: #350`).
    ///
    /// One sync per rAF. Do **not** End merely because `media_time <= 0` mid-timeline.
    /// Apply a prepared loop buffer if ready; otherwise cold-restart at frame 0.
    ///
    /// Prefer [`Self::prepare_loop_if_needed`] while the end still plays (same
    /// decoder, end frames already queued) so this is only a buffer swap.
    pub async fn loop_to_start(&self) -> Result<(), MediaError> {
        let mut waits = 0_u32;
        while self.shared.session.get().is_preparing_loop() && waits < 500 {
            sleep_ms(5).await;
            waits += 1;
        }

        if self.apply_loop_prime()? {
            return Ok(());
        }

        self.loop_to_start_cold().await
    }

    /// When the end of the clip is already in the presentation queue, reuse the
    /// main decoder to decode the start into `loop_prime` while presents drain
    /// the tail — no second `VideoDecoder`.
    ///
    /// Do **not** set `restarting` here — that freezes underrun hold and is for
    /// seek/cold restart only. Prefetch is gated by `preparing_loop` via
    /// [`Self::wants_play_prefetch`] / [`Self::prefetch_one`].
    pub async fn prepare_loop_if_needed(&self) -> Result<(), MediaError> {
        if !self.is_looping() || !self.is_playing() {
            return Ok(());
        }
        if self.shared.loop_prime.borrow().is_some()
            || self.shared.session.get().is_preparing_loop()
        {
            return Ok(());
        }
        if !self.presentation_reaches_eof() {
            return Ok(());
        }

        self.apply_session_event(crate::SessionEvent::BeginPrepareLoop);
        let epoch = self.shared.op_epoch.get();
        let result = self.fill_loop_prime(epoch).await;
        self.apply_session_event(crate::SessionEvent::EndPrepareLoop);
        result
    }

    pub(super) fn apply_loop_prime(&self) -> Result<bool, MediaError> {
        let Some(mut buf) = self.shared.loop_prime.borrow_mut().take() else {
            return Ok(false);
        };
        let Some(first) = buf.pop_front() else {
            return Ok(false);
        };

        self.clear_presentation();
        self.clear_step_ring();
        // Keep Covered + Exact across the loop seam — same clip, caches stay valid.
        self.present_and_cache(first.frame, first.video)?;
        self.touch_playhead(first.frame);
        self.trim_exact_for_play();
        self.shared.commit_frame(first.frame)?;
        *self.shared.presentation.borrow_mut() = buf;
        // Decoder is already warm past the primed start frames.
        Ok(true)
    }

    pub(super) async fn fill_loop_prime(&self, epoch: u64) -> Result<(), MediaError> {
        self.invalidate_decoder();
        let track = self.shared.track.clone();
        let count = self.state().frame_count;
        let n = (self
            .shared
            .track
            .metadata_ref()
            .play_policy()
            .loop_prime_frames() as FrameId)
            .clamp(1, count.max(1));
        let mut buf: VecDeque<QueuedFrame> = VecDeque::new();

        for frame in 0..n {
            if !self.is_playing() || !self.is_looping() || !self.is_current(epoch) {
                for q in buf.drain(..) {
                    drop(q.video);
                }
                return Ok(());
            }
            match self.decode_frame(&track, frame, epoch).await {
                Ok(video) => buf.push_back(QueuedFrame { frame, video }),
                Err(MediaError::Cancelled) => {
                    for q in buf.drain(..) {
                        drop(q.video);
                    }
                    return Ok(());
                }
                Err(e) => {
                    for q in buf.drain(..) {
                        drop(q.video);
                    }
                    return Err(e);
                }
            }
        }

        if !self.is_current(epoch) {
            for q in buf.drain(..) {
                drop(q.video);
            }
            return Ok(());
        }

        *self.shared.loop_prime.borrow_mut() = Some(buf);
        Ok(())
    }

    pub(super) async fn loop_to_start_cold(&self) -> Result<(), MediaError> {
        let epoch = self.begin_op();
        self.apply_session_event(crate::SessionEvent::BeginRestart);
        let result = async {
            self.clear_loop_prime();
            self.clear_presentation();
            self.clear_step_ring();
            // Same clip — do not wipe Covered/Exact (was: strip vanished every loop).
            self.invalidate_decoder();
            self.goto_frame(0, epoch).await?;
            self.touch_playhead(0);
            if self.is_playing() && self.is_current(epoch) {
                self.trim_exact_for_play();
                let ahead = self
                    .metadata()
                    .play_policy()
                    .loop_prime_frames()
                    .saturating_sub(1);
                self.prime_presentation(ahead, epoch).await?;
            }
            Ok(())
        }
        .await;
        self.apply_session_event(crate::SessionEvent::EndRestart);
        if !self.is_current(epoch) {
            return Err(MediaError::Cancelled);
        }
        result
    }

    /// Decode one frame into the queue. May run concurrently with [`Self::present_due`].
    ///
    /// No-ops while restarting or preparing a loop prime — those paths own the
    /// decoder (`prime_presentation` / `fill_loop_prime`).
    pub async fn prefetch_one(&self) -> Result<bool, MediaError> {
        if !self.is_playing()
            || !self.shared.play_session_live.get()
            || self.shared.play_loop_seam.get()
            || self.shared.resuming_clock.get()
            || self.shared.session.get().blocks_prefetch()
        {
            return Ok(false);
        }
        let epoch = self.shared.op_epoch.get();
        self.enqueue_next_presentation_frame(epoch).await
    }

    /// Push the next presentation-order frame (ignores the restarting gate).
    pub(super) async fn enqueue_next_presentation_frame(
        &self,
        epoch: u64,
    ) -> Result<bool, MediaError> {
        self.enqueue_presentation_frame(epoch, true).await
    }

    /// Decode the next presentation frame into the queue.
    ///
    /// `require_playing`: play/prefetch path drops work if paused mid-await.
    /// Scrub-settle seed (mediabunny `startVideoIterator` first two frames)
    /// passes `false` so frames land while still soft-paused for resume.
    async fn enqueue_presentation_frame(
        &self,
        epoch: u64,
        require_playing: bool,
    ) -> Result<bool, MediaError> {
        if self.presentation_len() >= self.play_presentation_cap() {
            return Ok(false);
        }
        let (track, next) = {
            let count = self.shared.track.frame_count();
            let next = self
                .shared
                .presentation
                .borrow()
                .back()
                .map(|q| q.frame.saturating_add(1))
                .unwrap_or_else(|| self.shared.frame.get().saturating_add(1));
            if next >= count {
                return Ok(false);
            }
            let track = self.shared.track.clone();
            // Stay ahead of mfra index so play never stalls at a chunk seam
            // (warm+batch ≈ every ~24 frames with chunk=2; was ~120 with chunk=8).
            // Stay ahead of mfra index so play never stalls at a chunk seam.
            if !track.fragments_complete() {
                let indexed = track.indexed_frame_count();
                let lead = self
                    .shared
                    .track
                    .metadata_ref()
                    .settle_policy()
                    .play_index_lead;
                if next.saturating_add(lead) >= indexed {
                    track.request_index_through(next.saturating_add(lead.saturating_mul(2)));
                }
                track.prefetch_samples_ahead(next, lead);
            }
            (track, next)
        };
        let video = match self.decode_frame(&track, next, epoch).await {
            Ok(v) => v,
            Err(MediaError::Cancelled) => return Ok(false),
            Err(e) => return Err(e),
        };
        if !self.is_current(epoch) || (require_playing && !self.is_playing()) {
            drop(video);
            return Ok(false);
        }
        self.shared
            .presentation
            .borrow_mut()
            .push_back(QueuedFrame { frame: next, video });
        Ok(true)
    }

    /// After Exact settle: fill the first presentation frames (mediabunny
    /// `startVideoIterator` awaits first two canvases before `play`). PlayPrime
    /// skips byte-warm + fill when this queue is already primed.
    pub(super) async fn seed_play_presentation_after_seek(
        &self,
        epoch: u64,
    ) -> Result<(), MediaError> {
        let target = self
            .shared
            .track
            .metadata_ref()
            .settle_policy()
            .play_prime_frames
            .min(self.play_presentation_cap());
        while self.presentation_len() < target {
            if !self.is_current(epoch) {
                return Err(MediaError::Cancelled);
            }
            match self.enqueue_presentation_frame(epoch, false).await {
                Ok(true) => {}
                Ok(false) => break,
                Err(MediaError::Cancelled) => return Err(MediaError::Cancelled),
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }

    /// Reverse: fill toward the past — sync key only (`mediabunny: #350`).
    pub async fn prefetch_until(&self, deadline_ms: f64) -> Result<usize, MediaError> {
        let mut decoded = 0usize;
        while self.is_playing() && self.presentation_len() < self.play_presentation_cap() {
            if let Some(now) = now_ms()
                && now + PREFETCH_DEADLINE_SLACK_MS >= deadline_ms
            {
                break;
            }
            if !self.prefetch_one().await? {
                break;
            }
            decoded += 1;
        }
        Ok(decoded)
    }

    pub(super) fn catch_up_frame(
        &self,
        next: FrameId,
        behind_seconds: f64,
    ) -> Result<FrameId, MediaError> {
        let index = self.shared.track.index();
        let next_entry = index.get(next).ok_or(MediaError::FrameOutOfRange(
            next,
            self.shared.track.frame_count(),
        ))?;
        let timescale = next_entry.timescale.max(1) as f64;
        let desired_ticks =
            next_entry.composition_timestamp_ticks as f64 + behind_seconds * timescale;

        let mut target = next;
        for entry in index.frames().iter().skip(next as usize) {
            if entry.composition_timestamp_ticks as f64 <= desired_ticks {
                target = entry.frame;
            } else {
                break;
            }
        }

        if behind_seconds >= BIG_LAG_SECONDS
            && let Some(key) = index.nearest_keyframe(target)
            && key >= next
        {
            return Ok(key);
        }
        Ok(target)
    }

    pub(super) async fn present_frame(&self, frame: FrameId) -> Result<(), MediaError> {
        {
            let mut presentation = self.shared.presentation.borrow_mut();
            if let Some(front) = presentation.front() {
                if front.frame == frame {
                    let queued = presentation.pop_front().expect("front checked");
                    drop(presentation);
                    self.present_for_mode(queued.frame, queued.video)?;
                    self.touch_playhead(queued.frame);
                    self.shared.commit_frame(queued.frame)?;
                    return Ok(());
                }
                if front.frame < frame {
                    drop(presentation);
                    self.drain_presentation_before(frame);
                    let mut presentation = self.shared.presentation.borrow_mut();
                    if presentation.front().is_some_and(|f| f.frame == frame) {
                        let queued = presentation.pop_front().expect("front checked");
                        drop(presentation);
                        self.present_for_mode(queued.frame, queued.video)?;
                        self.touch_playhead(queued.frame);
                        self.shared.commit_frame(queued.frame)?;
                        return Ok(());
                    }
                } else {
                    drop(presentation);
                    self.clear_presentation();
                    self.clear_step_ring();
                }
            }
        }
        self.goto_frame(frame, self.shared.op_epoch.get()).await
    }

    pub(super) async fn fill_presentation_queue(&self, epoch: u64) -> Result<(), MediaError> {
        // Start playback after a couple of frames; background prefetch fills to CAP.
        let min_frames = self
            .shared
            .track
            .metadata_ref()
            .settle_policy()
            .play_prime_frames;
        self.prime_presentation(min_frames, epoch).await
    }

    /// Decode up to `min_frames` into the presentation queue (bypasses restarting gate).
    pub(super) async fn prime_presentation(
        &self,
        min_frames: usize,
        epoch: u64,
    ) -> Result<(), MediaError> {
        let target = min_frames.min(self.play_presentation_cap());
        while self.presentation_len() < target {
            if !self.is_current(epoch) {
                return Err(MediaError::Cancelled);
            }
            match self.enqueue_next_presentation_frame(epoch).await {
                Ok(true) => {}
                Ok(false) => break,
                Err(MediaError::Cancelled) => return Err(MediaError::Cancelled),
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }

    pub(super) fn drain_presentation_before(&self, frame: FrameId) {
        let mut presentation = self.shared.presentation.borrow_mut();
        while presentation.front().is_some_and(|f| f.frame < frame) {
            if let Some(q) = presentation.pop_front() {
                drop(q.video);
            }
        }
    }
}
