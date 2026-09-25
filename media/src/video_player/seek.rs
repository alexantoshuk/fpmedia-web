//! Seek / step orchestration for [`super::VideoPlayer`].

use crate::engine_mode::{EngineEvent, reduce as engine_reduce};
use crate::error::MediaError;
use crate::types::FrameId;

use crate::decode::owned_video_frame::OwnedVideoFrame;

use super::VideoPlayer;
use super::scrub::{exact_scrub_ready, soft_cold_scrub_settle};

impl VideoPlayer {
    pub async fn seek_frame(&self, frame: FrameId) -> Result<(), MediaError> {
        let epoch = self.begin_op();
        // Exact VF — abort densify and hold so scrub HW cannot starve the cascade.
        // Densify blit is for scrub/step warm hits; seek always decodes VF.
        self.abort_scrub_fill(true);
        let (next, _) = engine_reduce(self.shared.engine.get(), EngineEvent::BeginExact);
        self.shared.engine.set(next);
        self.apply_session_event(crate::SessionEvent::BeginRestart);
        let result = async {
            self.shared.player.borrow_mut().pause();
            self.clear_loop_prime();
            self.clear_step_ring();
            self.clear_presentation();
            self.aim_scrub_cache(frame);
            // Cold seek on uncached / unindexed frame: do not block on
            // `goto_frame_with_interim` (same hang as cold scrub settle).
            if !exact_scrub_ready(&self.shared, frame) {
                soft_cold_scrub_settle(self, frame)?;
                self.shared.pending_cold_exact.set(Some(frame));
                Ok(())
            } else {
                self.shared.pending_cold_exact.set(None);
                // Keep last canvas until exact decode paints (same as scrub settle).
                self.goto_frame_with_interim(frame, epoch).await
            }
        }
        .await;
        self.apply_session_event(crate::SessionEvent::EndRestart);
        let (next, _) = engine_reduce(self.shared.engine.get(), EngineEvent::EndExact);
        self.shared.engine.set(next);
        // Exact seek can leave main decodeQ elevated; idle before densify.
        if !self.is_current(epoch) {
            return Err(MediaError::Cancelled);
        }
        result?;
        self.snap_media_clock_to_playhead();
        self.arm_scrub_densify_after_exact(self.state().frame);
        Ok(())
    }

    /// Seek to the nearest sync at or before `frame` (mediabunny `seekToKey`).
    /// Fast scrub preview: one key decode (or cache hit), then exact [`Self::seek_frame`].
    pub async fn seek_keyframe(&self, frame: FrameId) -> Result<(), MediaError> {
        let key = self
            .key_for(frame)
            .ok_or(MediaError::FrameOutOfRange(frame, self.state().frame_count))?;
        self.seek_frame(key).await
    }

    pub async fn next_frame(&self) -> Result<(), MediaError> {
        self.shared.player.borrow_mut().pause();
        let next = self.state().frame.saturating_add(1);
        if next >= self.state().frame_count {
            return Err(MediaError::FrameOutOfRange(next, self.state().frame_count));
        }
        // Bias densify along key-repeat / hold (predictive gap pick).
        self.note_scrub_direction(next);
        // Live ahead VF (play prefetch / prior Next).
        if let Some(video) = self.take_presentation_front(next) {
            self.install_displayed(next, video)?;
            self.shared.player.borrow_mut().commit_frame(next)?;
            self.touch_playhead(next);
            self.snap_media_clock_to_playhead();
            self.arm_scrub_densify_after_step(next);
            return Ok(());
        }
        // Densify warm blit (full-res VF→ImageBitmap). After a short-clip fill
        // this matches ShotGrid buffered step; skip key→N.
        // mediabunny: CanvasSink canvas-pool present without re-decode.
        if self.try_present_warm_blit(next)? {
            if let Some(old) = self.shared.displayed.borrow_mut().take() {
                self.push_step_behind(old);
            }
            self.shared.player.borrow_mut().commit_frame(next)?;
            self.touch_playhead(next);
            self.snap_media_clock_to_playhead();
            self.arm_scrub_densify_after_step(next);
            return Ok(());
        }
        // Blit miss: warm sequential `decode_frame` + ingest — do **not** abort
        // densify / latch exact-hold (that starved blit refill on ←/→ hold).
        // mediabunny: samples() / mediaSamplesInRange keep the range pump.
        self.step_soft_goto(next, false).await
    }

    pub async fn previous_frame(&self) -> Result<(), MediaError> {
        self.shared.player.borrow_mut().pause();
        let cur = self.state().frame;
        if cur == 0 {
            return Err(MediaError::FrameOutOfRange(0, self.state().frame_count));
        }
        let prev = cur - 1;
        self.note_scrub_direction(prev);
        // Live behind VF — no key→N.
        if let Some(video) = self.take_step_behind(prev) {
            if let Some(displayed) = self.shared.displayed.borrow_mut().take() {
                self.push_presentation_front(displayed);
            }
            self.install_displayed_no_retire(prev, video)?;
            self.shared.player.borrow_mut().commit_frame(prev)?;
            self.touch_playhead(prev);
            self.snap_media_clock_to_playhead();
            self.arm_scrub_densify_after_step(prev);
            return Ok(());
        }
        // Densify warm blit — same residency as Next.
        if self.try_present_warm_blit(prev)? {
            if let Some(displayed) = self.shared.displayed.borrow_mut().take() {
                self.push_presentation_front(displayed);
            }
            self.shared.player.borrow_mut().commit_frame(prev)?;
            self.touch_playhead(prev);
            self.snap_media_clock_to_playhead();
            self.arm_scrub_densify_after_step(prev);
            return Ok(());
        }
        // Miss: soft decode; `goto_frame_fill_behind` parks near-target VF for Prev.
        // mediabunny: mediaSamplesAtTimestamps restarts from key on rewind.
        self.step_soft_goto(prev, true).await
    }

    /// Next/Prev blit miss without settle-style densify abort.
    ///
    /// Keeps the scrub decoder filling blit ahead of aim while the main decoder
    /// warms the stepped frame (`present_and_cache` re-ingest).
    pub(super) async fn step_soft_goto(
        &self,
        frame: FrameId,
        fill_behind: bool,
    ) -> Result<(), MediaError> {
        self.aim_scrub_cache(frame);
        let epoch = self.begin_op();
        let result = if fill_behind {
            self.goto_frame_fill_behind(frame, epoch).await
        } else {
            self.goto_frame(frame, epoch).await
        };
        if !self.is_current(epoch) {
            return Err(MediaError::Cancelled);
        }
        result?;
        self.snap_media_clock_to_playhead();
        self.arm_scrub_densify_after_step(self.state().frame);
        Ok(())
    }

    /// Exact frame via decode (scrub store is markers only — no VF round-trip).
    pub async fn frame_at(&self, frame: FrameId) -> Result<OwnedVideoFrame, MediaError> {
        let count = self.state().frame_count;
        if frame >= count {
            return Err(MediaError::FrameOutOfRange(frame, count));
        }
        let epoch = self.begin_op();
        let track = self.shared.player.borrow().track_rc();
        self.decode_frame(&track, frame, epoch).await
    }

    /// Sparse timestamps API — filmstrip follow-up (mediabunny `samplesAtTimestamps`).
    pub async fn frames_at_timestamps(
        &self,
        _timestamps: &[f64],
    ) -> Result<Vec<Option<OwnedVideoFrame>>, MediaError> {
        Err(MediaError::Unsupported(
            "frames_at_timestamps — use mediabunny samplesAtTimestamps pattern in a follow-up",
        ))
    }
}
