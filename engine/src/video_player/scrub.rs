//! Scrub densify orchestration for [`super::VideoPlayer`]. Hover thumbs: `thumb.rs`.

use std::rc::Rc;

use crate::decode::owned_video_frame::OwnedVideoFrame;
use crate::decode::scrub_blit::draw_image_bitmap;
use crate::decode::webcodecs::WebCodecsDecoder;
use crate::demux::Mp4VideoTrack;
use crate::error::MediaError;
use crate::model::engine_mode::EngineEvent;
use crate::policy::scrub_plan::{densify_should_ban_after_no_progress, densify_skip_bridge_ids};
use crate::policy::{
    ScrubCachePolicy, ScrubPolicy, exact_frames_to_free_for_play_vram, scrub_blit_cap_during_play,
    scrub_blit_frame_budget, scrub_drag_stage_choice, scrub_drag_stage_near_aim,
};
use crate::residency::ScrubChunk;
use crate::types::FrameId;

use super::scrub_cold::{
    exact_scrub_ready, schedule_fetchmap_gap_fill, scrub_gop_bytes_warm, soft_cold_scrub_settle,
    try_resolve_pending_cold_exact, warm_scrub_gop_bytes,
};
use super::scrub_gap::{gop_sample_lead, next_gap_target, next_gap_unbanned, playhead_and_last};
use super::{
    PLAY_EXACT_VRAM_VF_SLACK, Shared, VideoPlayer, next_animation_frame, now_ms, sleep_ms,
    yield_microtask,
};

impl VideoPlayer {
    /// Aim + park only — no densify kick. Used while the main decoder owns an
    /// exact seek/scrub (`restarting`) so the scrub decoder does not compete for
    /// the HW session until the finger frame is on canvas.
    pub(super) fn aim_scrub_cache(&self, focus: FrameId) {
        // Mode is owned by [`EngineEvent`] (EnterScrub / BeginExact / EndExact /
        // EnterPlay). Aim only moves the eviction/gap focus — do not flip ExactHold
        // back to Scrub here (that re-enabled densify under settle).
        // Keep the step VF ring (displayed / behind / ahead) across Pause and
        // step — park used to drop ahead VFs and force key→N on Next/Prev hold.
        // Seek / scrub settle call [`Self::clear_step_ring`] explicitly.
        self.touch_playhead(focus);
        // Fresh aim — allow fill even after a prior stall.
        self.shared.fill_stalled_aim.set(None);
    }

    /// **Unified scrub-cache pipeline** — the only entry for pause, scrub pointer
    /// moves, scrub hold, scrub release, and post-seek/step.
    ///
    /// 1. Scrub fill mode (gap-fill on; play fill off)
    /// 2. Park play presentation queue into the dense cache
    /// 3. Aim eviction / gap-fill at `focus`
    /// 4. Kick bidirectional GOP densify until budget-saturated
    ///    (skipped when [`ScrubPolicy::densify`] is false — All-Intra;
    ///    visited frames still land in dense via `present_and_cache`)
    ///
    /// While [`EngineMode::ExactHold`] (scrub settle / sync seek), densify
    /// is deferred so exact `goto_frame` owns HW. Drag does **not** enter here —
    /// PreferSoftware densify under the finger starves pointer/rAF; densify arms
    /// after settle / hard Pause / step.
    pub(super) fn enter_scrub_cache_pipeline(&self, focus: FrameId) {
        self.shared.scrub_gesture.set(false);
        self.aim_scrub_cache(focus);
        if self.scrub_policy().densify {
            self.schedule_scrub_cache_fill();
        }
        // Gray FetchMap plane — independent of densify / `scrub_busy`.
        schedule_fetchmap_gap_fill(&self.shared);
        self.schedule_thumb_key_fill();
    }

    /// Tab / window hidden: stop densify / gray / thumbs. Play clock stays live.
    ///
    /// `mediabunny: examples/media-player/media-player.ts` — no visibility hold;
    /// audio+`AudioContext` clock continue. Local: scrub planes only (CPU/Ranges).
    /// Also deepens audio ahead ([`crate::AudioOutput::set_tab_hidden`]) and
    /// blocks video-underrun → [`AudioOutput::hold_underrun`] (that made
    /// background audio choppy when rAF/`nextFrame` lagged).
    pub fn yield_scrub_work_for_hidden(self: &Rc<Self>) {
        if self.shared.is_disposed() {
            return;
        }
        self.shared.scrub_bg_suspended.set(true);
        self.abort_scrub_fill(false);
        self.cancel_hover_thumb();
        if let Ok(slot) = self.shared.audio.try_borrow()
            && let Some(a) = slot.as_ref()
        {
            a.set_tab_hidden(true);
        }
        // Already silenced for video underrun — restart now; do not wait for
        // `nextFrame` (may stay empty while rAF is throttled).
        if self.shared.clock_state.get().underrun_holding()
            && self.is_playing()
            && !self.shared.resuming_clock.get()
        {
            self.spawn_resume_after_underrun_hold();
        }
    }

    /// Tab visible again: clear suspend and re-arm Pause densify / gray / thumbs.
    pub fn resume_scrub_work_after_visible(&self) {
        if self.shared.is_disposed() {
            return;
        }
        self.shared.scrub_bg_suspended.set(false);
        if let Ok(slot) = self.shared.audio.try_borrow()
            && let Some(a) = slot.as_ref()
        {
            a.set_tab_hidden(false);
        }
        // Playing: suppress underrun→audio hold while video skip-catches.
        if self.is_playing() || self.play_session_live() {
            self.arm_underrun_catchup_grace();
            return;
        }
        if self.shared.scrub_gesture.get() || self.shared.engine.get().holds_exact() {
            return;
        }
        self.enter_scrub_cache_pipeline(self.state().frame);
    }

    pub(super) fn scrub_policy(&self) -> ScrubPolicy {
        self.shared.track.metadata_ref().scrub_policy()
    }

    /// Stop densify immediately so Exact / seek can own decode.
    ///
    /// Long-GOP settle must not compete with exact cascade on the same GPU
    /// (mediabunny: seekToTime terminates the prior iterator before a new range;
    /// densify plane is local — yield scrub decoder fully before Exact).
    /// `rearm` → latch `scrub_pending` so fill resumes after exact clears hold.
    ///
    /// When the scrub decoder is idle in the slot: `reset` and **put back** (do
    /// not Drop/close — that raced densify `decode()` → closed codec). When
    /// densify has it checked out, only bump epoch; densify aborts via cancel.
    pub(super) fn abort_scrub_fill(&self, rearm: bool) {
        // Latch `scrub_pending` *before* bumping the epoch so densify's adopt
        // check (`!scrub_pending` → exit for Play) cannot race a settle rearm.
        self.shared.densify.set_pending(rearm);
        let scrub = self.shared.scrub_epoch.get().wrapping_add(1);
        self.shared.scrub_epoch.set(scrub);
        // Take under a short borrow — do not `if let … borrow_mut().take()` (RefMut
        // spans the body; `reset` / Drop re-entry panics).
        let taken = self.shared.scrub_decoder.borrow_mut().take();
        if let Some(mut d) = taken {
            let _ = d.reset();
            *self.shared.scrub_decoder.borrow_mut() = Some(d);
        }
    }

    /// After [`Self::abort_scrub_fill`], wait until densify clears `scrub_busy`.
    ///
    /// Exact/seek/PlayPrime must not start while `decode_gop` still feeds the scrub
    /// decoder (`mediabunny: seekToTime` → `startVideoIterator` terminates the
    /// prior iterator immediately — no densify plane). Local dual-decoder: brief
    /// yield then Exact/fill; long URL Range stalls must not block for seconds.
    /// Poison recovery: Exact soft-cold / recreate + PlayPrime fill recreate.
    /// If still busy after the first window, bump abort again and drain once more.
    ///
    /// `rearm`: settle/seek → `true` (densify resumes after Exact); PlayPrime →
    /// `false` (Play must not keep filling gaps).
    pub(super) async fn await_scrub_densify_yield(&self, rearm: bool) {
        let policy = self.shared.track.metadata_ref().settle_policy();
        let budget_ms = policy.densify_yield_ms;
        let retry_ms = policy.densify_yield_retry_ms;
        let start = now_ms().unwrap_or(0.0);
        while self.shared.densify.is_busy() {
            let now = now_ms().unwrap_or(start);
            if now - start >= budget_ms {
                break;
            }
            sleep_ms(0).await;
        }
        if !self.shared.densify.is_busy() {
            return;
        }
        web_sys::console::warn_1(&"fpmedia-web: densify still busy after yield — re-abort".into());
        self.abort_scrub_fill(rearm);
        let start2 = now_ms().unwrap_or(0.0);
        while self.shared.densify.is_busy() {
            let now = now_ms().unwrap_or(start2);
            if now - start2 >= retry_ms {
                break;
            }
            sleep_ms(0).await;
        }
    }

    /// Update predictive densify direction from a drag sample (`sign(aim − prev)`).
    pub(super) fn note_scrub_direction(&self, aim: FrameId) {
        if let Some(prev) = self.shared.prev_scrub_aim.get() {
            if aim > prev {
                self.shared.scrub_dir.set(1);
            } else if aim < prev {
                self.shared.scrub_dir.set(-1);
            }
            // Equal aim: keep last dir (micro-jitter / hold).
        }
        self.shared.prev_scrub_aim.set(Some(aim));
    }

    /// Neutral gap pick after settle / play (no motion bias).
    pub(super) fn clear_scrub_direction(&self) {
        self.shared.scrub_dir.set(0);
        self.shared.prev_scrub_aim.set(None);
    }

    /// Pointer-up settle entry from the UI host — before the async op runs.
    ///
    /// Must not call [`Self::note_scrub_frame`] / densify kick: that starved exact
    /// on long-GOP long GOPs (playhead far, `eng` stuck at 0, `decodeQ≈10`).
    pub fn prepare_scrub_settle(&self, frame: FrameId) {
        self.cancel_hover_thumb();
        // BeginExact → AbortDensify { rearm: true }.
        self.apply_engine_event(EngineEvent::BeginExact);
        self.clear_scrub_direction();
        self.cancel_ops();
        // Drop in-flight main-decoder cascade from the preceding drag sample so
        // settle does not inherit a wedged `decodeQueueSize`.
        self.invalidate_decoder();
        self.clear_step_ring();
        self.clear_presentation();
        self.aim_scrub_cache(frame);
        // Do **not** trim dense here — wiping the strip on pointer-up felt like
        // the cache vanishing even when releasing on a warm region.
    }

    /// Drag sample entry — clear ExactHold without re-arming densify.
    ///
    /// `prepare_scrub_settle` may have latched hold; if settle is superseded by a
    /// new drag before `scrub_to` clears it, ExactHold would block densify until
    /// Pause. Densify stays off for the drag (PreferSoftware under finger lags);
    /// settle / hard Pause re-arms.
    pub fn clear_densify_hold(&self) {
        if !self.shared.engine.get().holds_exact() {
            return;
        }
        // Clear ExactHold without densify — Drag keeps PreferSoftware fill off.
        self.apply_engine_event(EngineEvent::EndExact { densify: false });
    }

    /// Scrub pointer sample (drag / hold) — aim + direction only.
    ///
    /// Does **not** kick densify: PreferSoftware fill under the finger saturates
    /// the JS thread so the bar lags the cursor. Abort any in-flight Pause densify
    /// for this gesture; settle / hard Pause / step re-arm via
    /// [`Self::enter_scrub_cache_pipeline`]. Updates [`Shared::drag_aim`] so an
    /// in-flight soft-drag Exact can chase without `cancel_ops` on every move.
    pub fn note_scrub_frame(&self, frame: FrameId) {
        self.cancel_hover_thumb();
        self.shared.scrub_gesture.set(true);
        self.shared.drag_aim.set(frame);
        self.note_scrub_direction(frame);
        self.aim_scrub_cache(frame);
        if self.shared.densify.is_busy() || self.shared.densify.is_pending() {
            self.abort_scrub_fill(false);
        }
    }

    /// True when Exact blit already holds `frame` (no paint). Host move path uses
    /// this to skip dwell without sync-present fighting ScrubDrag.
    /// **no equivalent** (scrub local).
    pub fn has_scrub_exact_blit(&self, frame: FrameId) -> bool {
        self.shared
            .scrub_blit
            .try_borrow()
            .map(|b| b.contains(frame))
            .unwrap_or(false)
    }

    /// Present Exact blit for `frame` when already cached (dwell / settle / pointer-down).
    ///
    /// Returns `true` when the stage was painted. Do **not** call on every
    /// pointermove — fights throttled ScrubDrag (Exact-strip flicker).
    /// **no equivalent** (scrub local).
    pub fn try_present_scrub_exact(&self, frame: FrameId) -> bool {
        match self.try_present_warm_blit(frame) {
            Ok(true) => self.shared.commit_frame(frame).is_ok(),
            Ok(false) | Err(_) => false,
        }
    }

    /// Async timeline scrub (local scrub policy; settle always exact VF).
    ///
    /// - **Drag** (`settle=false`): chase nearest **key** for [`Shared::drag_aim`]
    ///   on main HW (1 packet). No Exact under the finger — that queued supersede
    ///   paints and “caught up” after stop. Densify off; no `cancel_ops` per move.
    ///   Exact = dwell / settle. Cold / densify-busy → blit / yield. **no equivalent**
    ///   (scrub local).
    /// - **Settle** (`settle=true`, pointer up): abort densify, then one key→exact
    ///   cascade (`goto_frame_with_interim`) so canvas gets decode `VideoFrame`
    ///   (not a soft still). Host arms densify after Exact.
    ///   Exact shape ≈ `mediabunny: samplesAtTimestamps` / settle path.
    pub async fn scrub_to(&self, frame: FrameId, settle: bool) -> Result<(), MediaError> {
        let epoch = self.begin_op();
        let densify_yield_ms = if settle {
            // Host may already have called [`Self::prepare_scrub_settle`]; reinforce.
            // Abort before yield so densify can drain; BeginExact rearms AbortDensify.
            self.abort_scrub_fill(true);
            let yield_t0 = now_ms().unwrap_or(0.0);
            self.await_scrub_densify_yield(true).await;
            let yield_ms = now_ms().unwrap_or(yield_t0) - yield_t0;
            self.apply_engine_event(EngineEvent::BeginExact);
            self.clear_scrub_direction();
            self.apply_session_event(crate::SessionEvent::BeginRestart);
            self.aim_scrub_cache(frame);
            yield_ms
        } else {
            // Drag: ExactHold → Scrub; aim + direction only (no densify under finger).
            let mut engine = self.shared.engine.get();
            engine.mode = crate::EngineMode::Scrub;
            self.shared.engine.set(engine);
            self.apply_session_event(crate::SessionEvent::BeginRestart);
            self.note_scrub_direction(frame);
            self.aim_scrub_cache(frame);
            0.0
        };
        let result = async {
            self.shared.playing.set(false);
            self.clear_play_session();

            if settle {
                // Exact needs the nearest key verified — not the whole table.
                // Full `await_sync_verify_complete` on URL walks every sync via
                // raw sample-sized Ranges (minutes of stall; gray strip already
                // warm from CachedSource). Decode cascade calls
                // `ensure_key_verified` on retrieve (mediabunny verifyKeyPackets).
                // Keep the last drag frame on canvas until exact decode VF paints.
                // Do **not** blit key/preview from store — key is often far from
                // target and flashes a wrong frame; store is low-q drag trail only.
                // Reinforce ring clear when host skipped [`Self::prepare_scrub_settle`].
                self.clear_step_ring();
                self.clear_presentation();
                // Cold click / tap on uncached or not-yet-indexed fMP4 frame: do
                // **not** `goto_frame_with_interim` (ensure_frame_indexed + key→N
                // on cold Ranges freezes the tab). Soft present + prefetch; densify
                // paints when `FetchMap` covers the GOP.
                //
                // Densify still busy after yield: skip Exact on the main decoder —
                // dual-HW race → Decoding error / backpressure (stress HEVC/60).
                // mediabunny: seekToTime → await startVideoIterator →
                // `await videoFrameIterator?.return()` before a new iterator
                // (examples/media-player/media-player.ts). Local densify plane:
                // soft-cold until scrub HW yields; densify rearms after EndExact.
                let densify_busy = self.shared.densify.is_busy();
                if densify_busy {
                    web_sys::console::warn_1(
                        &format!(
                            "fpmedia-web: settle densify busy after yield ({densify_yield_ms:.0}ms) → soft-cold (skip Exact)"
                        )
                        .into(),
                    );
                }
                if densify_busy || !exact_scrub_ready(&self.shared, frame) {
                    // Soft-present immediately, then await **one GOP** of sample
                    // bytes and Exact+seed when densify has yielded and bytes warm.
                    // Empty 400ms poll left soft-cold → PlayPrime `warm_sample_bytes(24)`
                    // (7–16s on cold URL).
                    soft_cold_scrub_settle(self, frame)?;
                    self.shared.pending_cold_exact.set(Some(frame));
                    let warm_t0 = now_ms().unwrap_or(0.0);
                    let key = self.key_for(frame).unwrap_or(frame);
                    warm_scrub_gop_bytes(&self.shared, key);
                    {
                        let track = self.shared.track.clone();
                        let lead = gop_sample_lead(&track, key);
                        if let Err(e) = track.warm_sample_bytes(key, lead).await {
                            if e.is_cancelled() {
                                return Err(e);
                            }
                            web_sys::console::warn_1(
                                &format!("fpmedia-web: settle gop-warm: {e}").into(),
                            );
                        }
                    }
                    if !self.is_current(epoch) {
                        return Err(MediaError::Cancelled);
                    }
                    let warm_ms = now_ms().unwrap_or(warm_t0) - warm_t0;
                    // Densify may have cleared during gop-warm — Exact only then.
                    if !self.shared.densify.is_busy() && exact_scrub_ready(&self.shared, frame) {
                        self.shared.pending_cold_exact.set(None);
                        self.settle_exact_seed_logged(
                            frame,
                            epoch,
                            densify_yield_ms,
                            Some(warm_ms),
                        )
                        .await?;
                    } else {
                        web_sys::console::log_1(
                            &format!(
                                "fpmedia-web: settle soft-cold (densify_yield={densify_yield_ms:.0}ms gop_warm={warm_ms:.0}ms densify_busy={} still cold={})",
                                self.shared.densify.is_busy(),
                                !exact_scrub_ready(&self.shared, frame),
                            )
                            .into(),
                        );
                    }
                } else {
                    self.shared.pending_cold_exact.set(None);
                    self.settle_exact_seed_logged(frame, epoch, densify_yield_ms, None)
                        .await?;
                }
                Ok(())
            } else {
                // Soft-drag: key preview via same floor/ceil + key-vs-cache pick as
                // interim (`scrub_drag_stage_choice`). Blind key present flashed
                // behind a warmer Exact blit. Exact = dwell/settle.
                // **no equivalent** (scrub local).
                self.shared.drag_aim.set(frame);
                loop {
                    if !self.is_current(epoch) {
                        return Err(MediaError::Cancelled);
                    }
                    let aim = self.shared.drag_aim.get();
                    let key = self.key_for(aim).unwrap_or(aim);
                    let dir = self.shared.scrub_dir.get();
                    let displayed = self
                        .shared
                        .displayed
                        .borrow()
                        .as_ref()
                        .map(|q| q.frame);
                    let blit_cached = {
                        let blit = self.shared.scrub_blit.borrow();
                        blit.nearest_frame_with_dir(aim, dir)
                    };
                    let stage = scrub_drag_stage_choice(aim, key, blit_cached, displayed, dir);

                    if let Some(id) = stage
                        && self.present_from_cache(id).await?
                    {
                        self.shared.commit_frame(id)?;
                        // Land only when near aim — same-GOP far ceil froze
                        // backward Exact-strip scrub (stale displayed/blit).
                        if scrub_drag_stage_near_aim(aim, key, id) {
                            if id == aim {
                                if self.shared.drag_aim.get() == aim {
                                    return Ok(());
                                }
                            } else if self.key_for(self.shared.drag_aim.get()).unwrap_or(aim)
                                == key
                            {
                                return Ok(());
                            }
                        }
                        continue;
                    }

                    if !exact_scrub_ready(&self.shared, key) {
                        return Ok(());
                    }
                    if self.shared.densify.is_busy() {
                        sleep_ms(0).await;
                        continue;
                    }

                    // Near warm preview (not key) — skip key decode. Far stage
                    // already rejected by near_aim; fall through to key chase.
                    if stage
                        .is_some_and(|id| id != key && scrub_drag_stage_near_aim(aim, key, id))
                    {
                        return Ok(());
                    }

                    match self.drag_key_chase(key, epoch).await {
                        Ok(true) => {
                            if self.key_for(self.shared.drag_aim.get()).unwrap_or(aim) == key {
                                return Ok(());
                            }
                        }
                        Ok(false) => {}
                        Err(e) if e.is_cancelled() => return Err(e),
                        Err(e) => {
                            web_sys::console::warn_1(
                                &format!("fpmedia-web: drag key skipped ({e})").into(),
                            );
                            return Ok(());
                        }
                    }
                }
            }
        }
        .await;
        self.apply_session_event(crate::SessionEvent::EndRestart);
        // End ExactHold; densify arm is host-gated on settle (after PlayPrime visible).
        self.apply_engine_event(EngineEvent::EndExact { densify: false });
        // Cancelled (superseded drag / seek): do not kick densify — a following
        // PlayPrime must keep a warm main decoder when possible.
        if !self.is_current(epoch) {
            return Err(MediaError::Cancelled);
        }
        if settle {
            // Densify is armed by the host *after* the drain loop can see a queued
            // PlayPrime — otherwise invalidate + scrub HW kick cause play-after-scrub
            // lag on long GOPs. Drag never densifies here (PreferSoftware under
            // finger lags the bar).
            // Do **not** `schedule_scrub_cache_fill` here when Play is pending — host
            // skips `arm_scrub_densify_after_exact` on purpose; Pause restarts fill.
            // Cold soft-settle: kick densify so Exact/`pending_cold_exact` can resolve
            // when bytes arrive (host may skip arm after settle).
            if self.shared.pending_cold_exact.get().is_some() {
                self.enter_scrub_cache_pipeline(frame);
            }
        }
        // Drag: no densify kick — settle / hard Pause / step arm the pipeline.
        result?;
        self.snap_media_clock_to_playhead();
        Ok(())
    }

    /// Pointer-up while resuming Play (`ScrubRelease { then_play }`): commit the
    /// aim **without** an Exact decode — PlayPrime's iterator draws it.
    ///
    /// `mediabunny: examples/media-player seekToTime` while playing: `pause`,
    /// `playbackTimeAtStart = seconds`, `startVideoIterator`, then `play` arms the
    /// clock. An Exact key→N here decoded the GOP twice and let a prewarmed audio
    /// clock run ahead of the first Play frame (skip burst on resume).
    pub fn prepare_play_from_scrub(&self, frame: FrameId) -> Result<FrameId, MediaError> {
        let _ = self.begin_op();
        let frame = frame.min(self.indexed_frame_count().saturating_sub(1));
        self.shared.playing.set(false);
        self.clear_play_session();
        self.clear_scrub_direction();
        self.clear_step_ring();
        self.clear_presentation();
        self.shared.pending_cold_exact.set(None);
        self.aim_scrub_cache(frame);
        self.shared.commit_frame(frame)?;
        self.apply_engine_event(EngineEvent::EndExact { densify: false });
        self.snap_media_clock_to_playhead();
        Ok(frame)
    }

    /// Exact goto with poison → soft-cold fallback. Shared by warm and gop-warm
    /// cold settle paths. Play after settle starts its own iterator (PlayPrime).
    async fn settle_exact_seed_logged(
        &self,
        frame: FrameId,
        epoch: u64,
        densify_yield_ms: f64,
        gop_warm_ms: Option<f64>,
    ) -> Result<(), MediaError> {
        let exact_t0 = now_ms().unwrap_or(0.0);
        match self.exact_goto_with_decoder_retry(frame, epoch).await {
            Ok(()) => {
                let exact_ms = now_ms().unwrap_or(exact_t0) - exact_t0;
                let warm_part = gop_warm_ms
                    .map(|w| format!(" gop_warm={w:.0}ms"))
                    .unwrap_or_default();
                web_sys::console::log_1(
                    &format!(
                        "fpmedia-web: settle Exact densify_yield={densify_yield_ms:.0}ms{warm_part} exact={exact_ms:.0}ms"
                    )
                    .into(),
                );
                Ok(())
            }
            Err(e) if e.is_cancelled() => Err(e),
            Err(e) if e.is_decoder_poison() => {
                let exact_ms = now_ms().unwrap_or(exact_t0) - exact_t0;
                web_sys::console::warn_1(
                    &format!(
                        "fpmedia-web: Exact poison → soft cold settle ({e}); densify_yield={densify_yield_ms:.0}ms exact={exact_ms:.0}ms"
                    )
                    .into(),
                );
                soft_cold_scrub_settle(self, frame)?;
                self.shared.pending_cold_exact.set(Some(frame));
                Ok(())
            }
            Err(e) => Err(e),
        }
    }

    /// After exact settle / sync seek: densify without killing the warm main
    /// decoder left by key→N — PlayPrime wants that cascade, not a cold reset.
    ///
    /// Densify runs on the **scrub** decoder; main `decodeQ` was already cleared
    /// in [`Self::prepare_scrub_settle`] before Exact goto. Do **not**
    /// `invalidate_decoder` here (was: play-after-scrub Decoding error / dead
    /// prime). Parity with [`Self::arm_scrub_densify_after_step`].
    /// `mediabunny: examples/media-player seekToTime` → keep iterator; play()
    /// without an extra terminate after the settle decode.
    pub fn arm_scrub_densify_after_exact(&self, focus: FrameId) {
        self.enter_scrub_cache_pipeline(focus);
    }

    /// After sequential Next/Prev: densify without killing the warm main decoder
    /// / step VF ring (`mediabunny: samples()` keep iterator; no flush per step).
    pub(super) fn arm_scrub_densify_after_step(&self, focus: FrameId) {
        self.enter_scrub_cache_pipeline(focus);
    }

    /// Best interim canvas frame while exact `target` is missing from Exact blit.
    ///
    /// Warm blit needs a live `ImageBitmap` — pick among Exact residents only.
    /// Covered markers are gap geometry / strip; using them here chose ids with
    /// no bitmap → silent no-op during soft-drag (worse after dense Covered +
    /// Exact orphan prune). Directional floor/ceil via `scrub_dir` so settle
    /// Exact does not jump opposite the drag; else nearest sync **if** it still
    /// has an Exact bitmap.
    pub(super) fn scrub_preview_frame(&self, target: FrameId) -> Option<FrameId> {
        let dir = self.shared.scrub_dir.get();
        let blit = self.shared.scrub_blit.borrow();
        let cached = blit.nearest_frame_with_dir(target, dir);
        let key = self.key_for(target).filter(|&k| blit.contains(k));
        crate::scrub_preview_choice_with_dir(target, key, cached, dir)
    }

    /// Runtime scrub-cache gate. `Off` closes the scrub decoder and drops
    /// Covered + Exact; `On` re-arms the Pause pipeline (decoder recreated lazily).
    pub fn set_scrub_cache_policy(&self, policy: ScrubCachePolicy) {
        if self.shared.scrub_cache_policy.replace(policy) == policy {
            return;
        }
        match policy {
            ScrubCachePolicy::Off => {
                // Checked-out densify sees the epoch bump and drops the decoder
                // on return (`return_scrub_decoder`) — never close mid-`decode()`.
                self.abort_scrub_fill(false);
                drop(self.shared.scrub_decoder.borrow_mut().take());
                if let Ok(mut cache) = self.shared.scrub_chunks.try_borrow_mut() {
                    cache.clear();
                }
                if let Ok(mut blit) = self.shared.scrub_blit.try_borrow_mut() {
                    blit.clear();
                }
            }
            ScrubCachePolicy::On => {
                if !self.is_playing() {
                    self.enter_scrub_cache_pipeline(self.state().frame);
                }
            }
        }
    }

    pub fn scrub_cache_policy(&self) -> ScrubCachePolicy {
        self.shared.scrub_cache_policy.get()
    }

    /// Drop Covered + Exact + thumbs (open / close teardown only — **not** loop).
    pub(super) fn clear_scrub_store(&self) {
        // Abort in-flight gap fill first so the fill task drops the decoder slot.
        let scrub = self.shared.scrub_epoch.get().wrapping_add(1);
        self.shared.scrub_epoch.set(scrub);
        let taken = self.shared.scrub_decoder.borrow_mut().take();
        if let Some(mut d) = taken {
            let _ = d.reset();
        }
        if let Ok(mut cache) = self.shared.scrub_chunks.try_borrow_mut() {
            cache.clear();
        }
        if let Ok(mut blit) = self.shared.scrub_blit.try_borrow_mut() {
            blit.clear();
        }
        self.clear_thumbs();
        self.shared.pending_cold_exact.set(None);
    }

    /// Park `video` into scrub residency: sync `ImageBitmap` + gap marker.
    ///
    /// `mediabunny:` CanvasSink — present from decoded sample / canvas pool, no
    /// intermediate compressed stills on the scrub critical path.
    pub(super) fn ingest_scrub_frame(&self, frame: FrameId, video: OwnedVideoFrame, replace: bool) {
        ingest_scrub_frame_shared(&self.shared, frame, video, replace);
    }

    /// Present a densify-warm frame: **ImageBitmap only** (VF→canvas transfer).
    ///
    /// Store markers are gap geometry / strip only — never paint a still onto the
    /// playback canvas. Miss → `Ok(false)` so callers decode a real `VideoFrame`.
    pub(super) async fn present_from_cache(&self, frame: FrameId) -> Result<bool, MediaError> {
        self.try_present_warm_blit(frame)
    }

    /// Present densify `ImageBitmap` only (VF→canvas transfer).
    ///
    /// Next/Prev and scrub-drag share this path after warm fill.
    pub(super) fn try_present_warm_blit(&self, frame: FrameId) -> Result<bool, MediaError> {
        let Ok(mut blit) = self.shared.scrub_blit.try_borrow_mut() else {
            return Ok(false);
        };
        let Some(bitmap) = blit.get(frame) else {
            return Ok(false);
        };
        draw_image_bitmap(&self.shared.canvas, &self.shared.ctx, bitmap)?;
        Ok(true)
    }

    pub(super) fn schedule_scrub_cache_fill(&self) {
        // Gate on play mode / densify-off policy (All-Intra).
        // Settle/seek: latch pending while ExactHold owns HW.
        if self.shared.is_disposed()
            || self.shared.scrub_bg_suspended()
            || self.shared.scrub_cache_off()
            || self.shared.engine.get().is_play()
            || self.is_playing()
            || !self.scrub_policy().densify
            || self.shared.scrub_densify_blocked.get()
            || self.shared.scrub_gesture.get()
        {
            return;
        }
        if self.shared.engine.get().holds_exact() || self.shared.densify.is_busy() {
            self.shared.densify.latch_pending();
            return;
        }
        self.kick_scrub_cache_fill();
    }

    pub(super) fn kick_scrub_cache_fill(&self) {
        if self.shared.is_disposed()
            || self.shared.scrub_bg_suspended()
            || self.shared.scrub_cache_off()
            || self.shared.engine.get().is_play()
            || self.is_playing()
            || !self.scrub_policy().densify
            || self.shared.scrub_densify_blocked.get()
            || self.shared.scrub_gesture.get()
            || self.shared.engine.get().holds_exact()
        {
            if !self.shared.is_disposed()
                && !self.shared.scrub_bg_suspended()
                && !self.shared.scrub_cache_off()
                && self.shared.engine.get().holds_exact()
            {
                self.shared.densify.latch_pending();
            }
            return;
        }
        if !self.shared.densify.request() {
            return;
        }
        let shared = Rc::clone(&self.shared);
        wasm_bindgen_futures::spawn_local(async move {
            let _ = scrub_cache_fill_task(shared).await;
        });
    }
}

pub(super) async fn scrub_cache_fill_task(shared: Rc<Shared>) -> Result<(), MediaError> {
    let mut scrub_epoch = shared.scrub_epoch.get();
    let result = async {
        let mut idle_passes = 0u8;
        // Keys that PreferSoftware densify finished with zero new residency —
        // keep seeking other gaps instead of hammering the same GOP forever.
        let mut banned_keys: std::collections::HashSet<FrameId> = std::collections::HashSet::new();
        loop {
            // Clip switch: exit permanently (do not adopt scrub_epoch like pause abort).
            if shared.is_disposed() {
                return Ok(());
            }
            if shared.scrub_bg_suspended()
                || shared.scrub_cache_off()
                || shared.engine.get().is_play()
                || shared.playing.get()
                || !shared.track.metadata_ref().scrub_policy().densify
                || shared.scrub_densify_blocked.get()
                // Drag owns the pointer — PreferSoftware densify under finger
                // both lags the bar and steals Pause/settle progress.
                || shared.scrub_gesture.get()
            {
                return Ok(());
            }
            // Settle / sync seek owns HW — exit so `scrub_busy` clears and the scrub
            // decoder is not pinned in a spin-wait (was starving exact on long GOPs).
            if shared.engine.get().holds_exact() {
                shared.densify.latch_pending();
                return Ok(());
            }
            // Epoch bump from `abort_scrub_fill`. `rearm=false` (Play) clears
            // `scrub_pending` — exit so `scrub_busy` drops for PlayPrime fill.
            // `rearm=true` (settle/seek) keeps pending → adopt and continue.
            // mediabunny: seekToTime → iterator.return() ends the prior range.
            if shared.scrub_epoch.get() != scrub_epoch {
                if shared.is_disposed()
                    || shared.engine.get().is_play()
                    || shared.playing.get()
                    || !shared.engine.get().allows_densify()
                    || !shared.densify.is_pending()
                {
                    return Ok(());
                }
                scrub_epoch = shared.scrub_epoch.get();
                // Do **not** clear `scrub_pending` — abort(rearm)/schedule may have
                // latched another pass while we were cancelled mid-GOP.
            }

            apply_clip_budgets(&shared);
            // Farthest-evict every densify tick — do not wait for Exact near-gap
            // None. Soft-drag trail often has holes (under blit cap), so the old
            // idle gate left orphans past a hole forever; Covered-only puts never
            // trigger Exact `put` eviction either. Drag densify is off.
            evict_exact_orphans_beyond_aim(&shared);
            let Some(target) = next_gap_unbanned(&shared, &banned_keys) else {
                // Honest stop — do not clear-pending/`continue` spin.
                evict_exact_orphans_beyond_aim(&shared);
                shared.densify.clear_pending();
                schedule_fetchmap_gap_fill(&shared);
                return Ok(());
            };

            // Cold open / URL: wait for GOP bytes + index (local densify plane).
            let key = shared
                .track
                .index()
                .nearest_keyframe(target)
                .unwrap_or(target);
            warm_scrub_gop_bytes(&shared, key);
            if !scrub_gop_bytes_warm(&shared, key) {
                // Bound cold waits — empty GOP / unindexed key used to rAF-spin
                // forever with `scrub_busy` and a frozen strip.
                idle_passes = idle_passes.saturating_add(1);
                if idle_passes >= 30 {
                    shared
                        .fill_stalled_aim
                        .set(Some(playhead_and_last(&shared).0));
                    schedule_fetchmap_gap_fill(&shared);
                    return Ok(());
                }
                next_animation_frame().await;
                continue;
            }
            try_resolve_pending_cold_exact(&shared);

            match fill_one_gap(Rc::clone(&shared), target, scrub_epoch).await {
                Ok(true) => {
                    idle_passes = 0;
                    try_resolve_pending_cold_exact(&shared);
                    // One rAF between GOPs — Chrome VRAM flash under densify burst.
                    // mediabunny: CanvasSink poolSize stays tiny; local yield only.
                    next_animation_frame().await;
                }
                Ok(false) => {
                    // Soft-ban on Exact only — see `densify_should_ban_after_no_progress`.
                    let ids = shared.track.index().gop_frame_ids(key).unwrap_or_default();
                    let aim = playhead_and_last(&shared).0;
                    let (blit_complete, refuse_all_missing) = {
                        let blit = shared.scrub_blit.borrow();
                        let blit_complete =
                            !ids.is_empty() && ids.iter().all(|id| blit.contains(*id));
                        let refuse_all_missing = !ids.is_empty()
                            && ids.iter().all(|&id| {
                                blit.contains(id)
                                    || (blit.len() >= blit.max()
                                        && blit.would_evict_closer_than(id, aim))
                            });
                        (blit_complete, refuse_all_missing)
                    };
                    let (ban, next_idle) = densify_should_ban_after_no_progress(
                        blit_complete,
                        refuse_all_missing,
                        idle_passes,
                        3,
                    );
                    idle_passes = next_idle;
                    if ban {
                        banned_keys.insert(key);
                    }
                    if next_gap_unbanned(&shared, &banned_keys).is_none() {
                        evict_exact_orphans_beyond_aim(&shared);
                        shared.densify.clear_pending();
                        schedule_fetchmap_gap_fill(&shared);
                        return Ok(());
                    }
                    sleep_ms(0).await;
                }
                Err(MediaError::Cancelled) => {
                    if shared.engine.get().holds_exact() {
                        shared.densify.latch_pending();
                        return Ok(());
                    }
                    if shared.engine.get().is_play()
                        || shared.playing.get()
                        || !shared.densify.is_pending()
                    {
                        return Ok(());
                    }
                    idle_passes = 0;
                    sleep_ms(0).await;
                }
                Err(_) => {
                    // Transient WebCodecs fault — retry a few times, then skip key.
                    idle_passes = idle_passes.saturating_add(1);
                    if idle_passes >= 3 {
                        banned_keys.insert(key);
                        idle_passes = 0;
                        if next_gap_unbanned(&shared, &banned_keys).is_none() {
                            evict_exact_orphans_beyond_aim(&shared);
                            schedule_fetchmap_gap_fill(&shared);
                            return Ok(());
                        }
                    }
                    sleep_ms(0).await;
                }
            }
        }
    }
    .await;

    if shared.is_disposed() {
        shared.densify.release_busy();
        shared.densify.clear_pending();
        return result;
    }
    // While exact settle/seek holds densify, leave pending latched and do
    // **not** respawn — a spawn_local spin froze the tab (long-GOP cold scrub).
    if shared.engine.get().holds_exact() {
        shared.densify.release_busy();
        return result;
    }

    let was_pending = shared.densify.finish();
    let want_again = was_pending
        && shared.engine.get().allows_densify()
        && !shared.playing.get()
        && !shared.scrub_gesture.get()
        && next_gap_target(&shared).is_some(); // banned set dies with the task

    if want_again {
        shared.densify.begin();
        let again = Rc::clone(&shared);
        wasm_bindgen_futures::spawn_local(async move {
            yield_microtask().await;
            if again.is_disposed() {
                again.densify.release_busy();
                return;
            }
            let _ = scrub_cache_fill_task(again).await;
        });
    }
    result
}

/// Drop Exact bitmaps farther from aim than the blit cap radius.
///
/// Same farthest-from-aim rule as `put`, without waiting for a put or for Exact
/// near-gap to go idle (under-cap soft-drag trails keep near holes).
fn evict_exact_orphans_beyond_aim(shared: &Shared) {
    if shared.scrub_gesture.get() {
        return;
    }
    let (playhead, last) = playhead_and_last(shared);
    let indexed_last = shared.track.indexed_frame_count().saturating_sub(1);
    let playhead = playhead.min(last.min(indexed_last));
    let Ok(mut blit) = shared.scrub_blit.try_borrow_mut() else {
        return;
    };
    blit.set_playhead(playhead);
    blit.evict_far_from_aim_when_idle();
}

pub(super) fn apply_clip_budgets(shared: &Shared) {
    let meta = shared.track.metadata();
    let policy = meta.scrub_policy();
    // Covered markers are uncapped for the open clip; Exact blit keeps the MiB cap.
    // Aim-centered blit: MiB wallet → frame count from canvas buffer (RGBA).
    // `mediabunny:` CanvasSink width/height/fit + poolSize (sized, bounded VRAM).
    let buf_w = shared.canvas.width().max(1);
    let buf_h = shared.canvas.height().max(1);
    let full = scrub_blit_frame_budget(buf_w, buf_h, policy.blit_mib);
    let blit_cap = if shared.engine.get().is_play() {
        let play_vfs = 1 + meta.play_policy().presentation_cap + PLAY_EXACT_VRAM_VF_SLACK;
        let reserve =
            exact_frames_to_free_for_play_vram(play_vfs, meta.width, meta.height, buf_w, buf_h);
        scrub_blit_cap_during_play(full, reserve)
    } else {
        full
    };
    shared.scrub_blit.borrow_mut().set_max(blit_cap);
}

/// Densify / scrub ingest: VF → sync `ImageBitmap` + resident marker.
///
/// `mediabunny:` CanvasSink canvas-pool present — keep live bitmaps, no still encode.
pub(super) fn ingest_scrub_bitmap_only(
    shared: &Rc<Shared>,
    frame: FrameId,
    video: &web_sys::VideoFrame,
    target_w: u32,
    target_h: u32,
    replace: bool,
) -> bool {
    if shared.scrub_cache_off() {
        return false;
    }
    let (bitmap, _width, _height) = match shared
        .encode_canvas
        .with_blit(|enc| enc.frame_to_bitmap(video, target_w.max(1), target_h.max(1)))
    {
        Ok(p) => p,
        Err(_) => return false,
    };

    let marker = ScrubChunk;
    let mut inserted_new = false;
    {
        let Ok(mut store) = shared.scrub_chunks.try_borrow_mut() else {
            bitmap.close();
            return false;
        };
        if replace {
            store.put_chunk(frame, marker);
        } else if store.has_frame(frame) {
            // Marker already resident (common after Play wiped blit) — still
            // refresh the ImageBitmap below.
        } else if store.insert_chunk(frame, marker) {
            inserted_new = true;
        } else {
            bitmap.close();
            return false;
        }
    }

    if let Ok(mut blit) = shared.scrub_blit.try_borrow_mut() {
        blit.set_playhead(shared.scrub_chunks.borrow().playhead());
        if blit.put(frame, bitmap) {
            return true;
        }
        // Exact at cap refused a far frame — keep Covered marker so densify
        // coverage can grow beyond the blit window (Play already keeps markers
        // after clearing Exact). `put` closed the bitmap.
        return inserted_new;
    }
    bitmap.close();
    // Borrow race on blit — still keep a newly inserted marker.
    inserted_new
}

pub(super) fn ingest_scrub_frame_shared(
    shared: &Rc<Shared>,
    frame: FrameId,
    video: OwnedVideoFrame,
    replace: bool,
) -> bool {
    let target_w = shared.canvas.width().max(1);
    let target_h = shared.canvas.height().max(1);
    let ok = ingest_scrub_bitmap_only(
        shared,
        frame,
        video.as_web_sys(),
        target_w,
        target_h,
        replace,
    );
    drop(video);
    ok
}

pub(super) async fn fill_one_gap(
    shared: Rc<Shared>,
    target: FrameId,
    scrub_epoch: u64,
) -> Result<bool, MediaError> {
    let key = shared
        .track
        .index()
        .nearest_keyframe(target)
        .unwrap_or(target);

    let cancelled = || {
        shared.is_disposed()
            || shared.scrub_epoch.get() != scrub_epoch
            || shared.engine.get().is_play()
            || shared.playing.get()
            || shared.engine.get().holds_exact()
    };
    if cancelled() {
        return Err(MediaError::Cancelled);
    }
    {
        let aim = playhead_and_last(&shared).0;
        let index = shared.track.index().clone();
        let ids = index.gop_frame_ids(key).unwrap_or_default();
        // Skip only when markers **and** live blit cover the GOP. Markers alone
        // after Play's blit wipe must not count as done.
        let cache = shared.scrub_chunks.borrow();
        let blit = shared.scrub_blit.borrow();
        let blit_complete = !ids.is_empty() && ids.iter().all(|id| blit.contains(*id));
        // Markers already solid for this GOP: skip re-decode when Exact is also
        // complete, or when every missing blit slot would be refused at cap
        // (would only shred a closer resident — same gate as `ScrubBlitLru::put`).
        if !ids.is_empty() && cache.gop_complete(&ids) && cache.has_frame(target) {
            if blit_complete {
                return Ok(false);
            }
            let refuse_all_missing = ids.iter().all(|&id| {
                blit.contains(id)
                    || (blit.len() >= blit.max() && blit.would_evict_closer_than(id, aim))
            });
            if refuse_all_missing {
                return Ok(false);
            }
        }
    }

    let meta = shared.track.metadata();
    let track = shared.track.clone();

    // Short borrow for take — never hold scrub_decoder RefMut across await/reset.
    // After clip dispose the slot is empty — do not recreate a scrub decoder.
    if shared.is_disposed() || shared.scrub_cache_off() {
        return Err(MediaError::Cancelled);
    }
    let taken = shared.scrub_decoder.borrow_mut().take();
    let mut decoder = match taken {
        Some(d) => d,
        None => match WebCodecsDecoder::create_scrub(&meta).await {
            Ok(d) => d,
            // HEVC HW-only: second prefer-hardware would dual-HW stall / poison
            // the main decoder (`Unsupported configuration`). Skip densify for
            // this clip; Exact settle / Play keep the sole HW session.
            Err(MediaError::CodecUnsupported(_)) => {
                shared.scrub_densify_blocked.set(true);
                return Ok(false);
            }
            Err(e) => return Err(e),
        },
    };
    if cancelled() {
        return_scrub_decoder(&shared, decoder, false);
        return Err(MediaError::Cancelled);
    }

    // Stream densify markers + blit as each remapped frame emerges so the ready
    // strip paints per-frame (not one tick-burst after the whole GOP).
    // Progress = new residency only (marker or Exact key). Refreshing an
    // already-resident blit after Play wipe must not reset `idle_passes` or
    // densify busy-spins PreferSoftware re-decode of the same island forever.
    let shared_for_cache = Rc::clone(&shared);
    let newly = std::rc::Rc::new(std::cell::Cell::new(0u32));
    let newly_cb = Rc::clone(&newly);
    let outputs = std::rc::Rc::new(std::cell::Cell::new(0u32));
    let outputs_cb = Rc::clone(&outputs);
    let mut on_frame = move |id: FrameId, video: OwnedVideoFrame| {
        outputs_cb.set(outputs_cb.get().saturating_add(1));
        let had_marker = shared_for_cache.scrub_chunks.borrow().has_frame(id);
        let had_blit = shared_for_cache.scrub_blit.borrow().contains(id);
        if ingest_scrub_frame_shared(&shared_for_cache, id, video, false)
            && (!had_marker || !had_blit)
        {
            newly_cb.set(newly_cb.get().saturating_add(1));
        }
    };
    // Fill order: `architecture.md` § Predictive densify.
    let decoded = decoder
        .decode_gop(&track, key, &cancelled, &mut on_frame)
        .await;
    if let Err(e) = decoded {
        return_scrub_decoder(&shared, decoder, true);
        return Err(e);
    }

    let outs = outputs.get();
    let gop_ids = shared.track.index().gop_frame_ids(key).unwrap_or_default();
    let salvaged = if outs > 0 {
        match salvage_gop_exact_blit(&shared, &mut decoder, &track, &gop_ids, &cancelled).await {
            Ok(n) => n,
            Err(e) => {
                return_scrub_decoder(&shared, decoder, true);
                return Err(e);
            }
        }
    } else {
        0
    };
    let n = newly.get().saturating_add(salvaged);

    let bridged = if outs > 0 {
        skip_bridge_gop_markers(&shared, &gop_ids)
    } else {
        0
    };
    let made_progress = n > 0 || bridged > 0;
    if !made_progress {
        let _ = decoder.reset();
        drop(decoder);
        Ok(false)
    } else if shared.is_disposed() || shared.scrub_cache_off() {
        drop(decoder);
        Ok(true)
    } else {
        *shared.scrub_decoder.borrow_mut() = Some(decoder);
        Ok(true)
    }
}

fn return_scrub_decoder(shared: &Shared, mut decoder: WebCodecsDecoder, reset: bool) {
    if reset {
        let _ = decoder.reset();
    }
    if !shared.is_disposed() && !shared.scrub_cache_off() {
        *shared.scrub_decoder.borrow_mut() = Some(decoder);
    }
}

/// PreferSoftware `decode_gop` may omit presentation ids — Exact key→N salvage.
/// `mediabunny: samplesAtTimestamps` (vs `samples()` / decode_gop range).
async fn salvage_gop_exact_blit(
    shared: &Rc<Shared>,
    decoder: &mut WebCodecsDecoder,
    track: &Rc<Mp4VideoTrack>,
    gop_ids: &[FrameId],
    is_cancelled: &dyn Fn() -> bool,
) -> Result<u32, MediaError> {
    let aim = playhead_and_last(shared).0;
    let missing_blit: Vec<FrameId> = {
        let blit = shared.scrub_blit.borrow();
        gop_ids
            .iter()
            .copied()
            .filter(|&id| {
                if blit.contains(id) {
                    return false;
                }
                !(blit.len() >= blit.max() && blit.would_evict_closer_than(id, aim))
            })
            .collect()
    };
    let mut n = 0u32;
    for id in missing_blit {
        if is_cancelled() {
            return Err(MediaError::Cancelled);
        }
        match decoder.decode_frame(track, id, is_cancelled).await {
            Ok(video) => {
                let had_marker = shared.scrub_chunks.borrow().has_frame(id);
                let had_blit = shared.scrub_blit.borrow().contains(id);
                if ingest_scrub_frame_shared(shared, id, video, false) && (!had_marker || !had_blit)
                {
                    n = n.saturating_add(1);
                }
            }
            Err(MediaError::Cancelled) => return Err(MediaError::Cancelled),
            Err(_) => {}
        }
    }
    Ok(n)
}

/// Covered markers for GOP ids still absent after Exact salvage (`outs > 0`).
fn skip_bridge_gop_markers(shared: &Shared, gop_ids: &[FrameId]) -> u32 {
    let missing = {
        let cache = shared.scrub_chunks.borrow();
        densify_skip_bridge_ids(gop_ids, |id| cache.has_frame(id))
    };
    if missing.is_empty() {
        return 0;
    }
    let Ok(mut cache) = shared.scrub_chunks.try_borrow_mut() else {
        return 0;
    };
    let mut bridged = 0u32;
    for id in missing {
        if cache.insert_chunk(id, ScrubChunk) {
            bridged = bridged.saturating_add(1);
        }
    }
    bridged
}
