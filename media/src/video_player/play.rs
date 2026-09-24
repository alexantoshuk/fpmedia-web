//! Play / present / prefetch / loop orchestration for [`super::VideoPlayer`].

use std::collections::VecDeque;
use std::rc::Rc;

use crate::error::MediaError;
use crate::playback_policy::{
    EofWall, eof_timeline_end, eof_wall_decision, loop_drain_frames, near_eof_by_frames,
};
use crate::types::{
    FrameId, exact_frames_to_free_for_play_vram, scrub_blit_cap_during_play,
    scrub_blit_frame_budget,
};

use super::{
    BIG_LAG_SECONDS, DuePresent, LOOP_NEAR_END_PRESENTATION_CAP_MAX, PLAY_EXACT_VRAM_VF_SLACK,
    PLAY_PRIME_FRAMES, PREFETCH_DEADLINE_SLACK_MS, QueuedFrame, SHUTTLE_PRESENTATION_QUEUE_CAP,
    VideoPlayer, performance_now_ms, sleep_ms,
};

impl VideoPlayer {
    pub(super) fn play_presentation_cap(&self) -> usize {
        if self.is_shuttle() {
            return SHUTTLE_PRESENTATION_QUEUE_CAP;
        }
        let policy = self.metadata().play_policy();
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
            .metadata()
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
        let policy = self.metadata().play_policy();
        let fps = self
            .metadata()
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
            && !self.shared.session.get().blocks_prefetch()
            && self.presentation_len() < self.play_presentation_cap()
    }

    /// Shrink Exact to leave GPU headroom for Play VF pool; keep near-playhead bitmaps.
    pub(super) fn trim_exact_for_play(&self) {
        let Ok(mut blit) = self.shared.scrub_blit.try_borrow_mut() else {
            return;
        };
        let meta = self.shared.player.borrow().metadata().clone();
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

    pub async fn step_playback(&self) -> Result<bool, MediaError> {
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
                self.shared.player.borrow_mut().commit_frame(queued.frame)?;
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
            let player = self.shared.player.borrow();
            let index = player.track().index();
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
            self.shared.player.borrow_mut().pause();
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
        let epoch = self.shared.op_epoch.get();
        // fMP4: arm a long index lead in bg, but only *await* one urgent batch
        // past the current index so Play click / PlayPrime cannot stall for
        // many RTTs (`mediabunny: first samples before play` — not the whole
        // warm→lead gap on the click turn).
        const PLAY_INDEX_LEAD: u64 = 96;
        const PLAY_INDEX_AWAIT_BATCH: u64 = 48; // ≈ urgent chunk × GOP12
        let head = self.state().frame;
        {
            let track = self.shared.player.borrow().track_rc();
            if !track.fragments_complete() {
                let want = head.saturating_add(PLAY_INDEX_LEAD);
                track.request_index_through(want);
                let indexed = track.indexed_frame_count();
                let await_through = want.min(indexed.saturating_add(PLAY_INDEX_AWAIT_BATCH));
                if await_through > indexed {
                    track.ensure_indexed_through(await_through).await?;
                    if !self.is_current(epoch) || !self.is_playing() {
                        return Err(MediaError::Cancelled);
                    }
                }
            }
        }
        // Block until lead `mdat` is cached — fire-and-forget prefetch alone left
        // cold seams while bg sync-verify / backfill stampeded Ranges.
        // mediabunny: URLSource min-load / first samples before the sink runs.
        let warm_secs = {
            let track = self.shared.player.borrow().track_rc();
            track.warm_sample_bytes(head, PLAY_INDEX_LEAD).await?;
            let last = head
                .saturating_add(PLAY_INDEX_LEAD)
                .min(track.indexed_frame_count().saturating_sub(1));
            track
                .index()
                .get(last)
                .map(|e| e.composition_timestamp().seconds)
                .unwrap_or(0.0)
        };
        if !self.is_current(epoch) || !self.is_playing() {
            return Err(MediaError::Cancelled);
        }
        if warm_secs > 0.0 {
            let audio = self.shared.audio.borrow().as_ref().map(Rc::clone);
            if let Some(a) = audio {
                a.warm_bytes_through(warm_secs).await?;
                if !self.is_current(epoch) || !self.is_playing() {
                    return Err(MediaError::Cancelled);
                }
            }
        }
        // Scrub store is markers + blit — play re-decodes into the presentation
        // queue. Do **not** trim scrub residency on Play — eviction is budget /
        // farthest-from-aim only (Pause densify paid for that trail).
        self.fill_presentation_queue(epoch).await
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
    /// Wall / muted shuttle: reset `MediaClock` + `wall_origin`. Audio-driven
    /// shuttle: soft-hold then `restart_warm` so `media_time()` follows again.
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
                if !shared.player.borrow().state().playing {
                    return;
                }
                let audio = shared.audio.borrow().as_ref().map(Rc::clone);
                if let Some(a) = audio
                    && let Err(e) = a.restart_warm(t).await
                {
                    web_sys::console::warn_1(
                        &format!("webmedia: clock snap restart_warm: {e}").into(),
                    );
                }
            });
        } else {
            self.shared.clock.borrow_mut().start(t);
            if let Some(now) = performance_now_ms() {
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
            self.shared.player.borrow_mut().pause();
            return Ok(DuePresent::Ended);
        }

        let last_end = {
            let player = self.shared.player.borrow();
            let index = player.track().index();
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
                player.metadata().duration().seconds,
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
                    self.shared.player.borrow_mut().pause();
                    self.clear_presentation();
                    self.clear_step_ring();
                    return Ok(DuePresent::Ended);
                }
            }
        }

        let mut did_draw = false;
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
            if next_also_due {
                let lag_ms = ((media_time_secs - start) * 1000.0).max(0.0);
                self.shared.last_behind_ms.set(Some(lag_ms));
                self.shared
                    .frames_skipped
                    .set(self.shared.frames_skipped.get().saturating_add(1));
                drop(queued.video);
                self.shared.player.borrow_mut().commit_frame(queued.frame)?;
                continue;
            }

            let lag_ms = ((media_time_secs - start) * 1000.0).max(0.0);
            if lag_ms > 1.0 {
                self.shared.last_behind_ms.set(Some(lag_ms));
            }
            // Play: Exact ingest of presented frame; densify stays off.
            self.present_play_frame(queued.frame, queued.video)?;
            self.touch_playhead(queued.frame);
            self.shared.player.borrow_mut().commit_frame(queued.frame)?;
            did_draw = true;
            break;
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
        self.shared.player.borrow_mut().commit_frame(first.frame)?;
        *self.shared.presentation.borrow_mut() = buf;
        // Decoder is already warm past the primed start frames.
        Ok(true)
    }

    pub(super) async fn fill_loop_prime(&self, epoch: u64) -> Result<(), MediaError> {
        self.invalidate_decoder();
        let track = self.shared.player.borrow().track_rc();
        let count = self.state().frame_count;
        let n =
            (self.metadata().play_policy().loop_prime_frames() as FrameId).clamp(1, count.max(1));
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
        if !self.is_playing() || self.shared.session.get().blocks_prefetch() {
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
            let player = self.shared.player.borrow();
            let count = player.state().frame_count;
            let next = self
                .shared
                .presentation
                .borrow()
                .back()
                .map(|q| q.frame.saturating_add(1))
                .unwrap_or_else(|| player.state().frame.saturating_add(1));
            if next >= count {
                return Ok(false);
            }
            let track = player.track_rc();
            // Stay ahead of mfra index so play never stalls at a chunk seam
            // (warm+batch ≈ every ~24 frames with chunk=2; was ~120 with chunk=8).
            // Stay ahead of mfra index so play never stalls at a chunk seam.
            if !track.fragments_complete() {
                let indexed = track.indexed_frame_count();
                let lead = 96u64;
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

    /// After exact settle while soft-held for play resume: fill the first
    /// presentation frames (mediabunny `startVideoIterator` awaits first two
    /// canvases before `play`). PlayPrime then skips a cold re-prime.
    pub(super) async fn seed_play_presentation_after_seek(
        &self,
        epoch: u64,
    ) -> Result<(), MediaError> {
        let target = PLAY_PRIME_FRAMES.min(self.play_presentation_cap());
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
            if let Some(now) = performance_now_ms()
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
        let player = self.shared.player.borrow();
        let index = player.track().index();
        let next_entry = index.get(next).ok_or(MediaError::FrameOutOfRange(
            next,
            player.state().frame_count,
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
                    self.shared.player.borrow_mut().commit_frame(queued.frame)?;
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
                        self.shared.player.borrow_mut().commit_frame(queued.frame)?;
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
        self.prime_presentation(PLAY_PRIME_FRAMES, epoch).await
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
