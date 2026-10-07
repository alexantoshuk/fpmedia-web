//! Canvas present + presentation/step rings for [`super::VideoPlayer`].
//!
//! Split from `mod.rs` for navigation only — no behavior change (P3).
//! `mediabunny:` CanvasSink-style present; play vs scrub ingest paths.

use crate::decode::canvas::{configure_canvas_for_video, draw_frame_with_context};
use crate::decode::owned_video_frame::OwnedVideoFrame;
use crate::error::MediaError;
use crate::types::{FrameId, coalesce_frame_runs};

use super::scrub::{apply_clip_budgets, ingest_scrub_bitmap_only};
use super::shared::{QueuedFrame, STEP_BEHIND_CAP, STEP_BEHIND_CAP_PLAY};
use super::{PipelineStats, ReadyFrameKind, ReadyFrameMark, VideoPlayer, ready_kind_paint_order};

impl VideoPlayer {
    /// Recompute canvas buffer from current CSS layout × DPR (call on container resize).
    ///
    /// Changing `canvas.width`/`height` clears the bitmap (fullscreen ↔ windowed).
    /// Keep scrub **markers** + **ImageBitmap** LRU — they scale via `drawImage` dw/dh
    /// (old WebP still-store needed a wipe; blit does not). `mediabunny:` no densify
    /// wipe on resize; CanvasSink-style present redraw.
    pub fn relayout(&self) -> Result<(), MediaError> {
        let meta = self.metadata();
        let before = (self.shared.canvas.width(), self.shared.canvas.height());
        configure_canvas_for_video(&self.shared.canvas, meta.width, meta.height)?;
        // Recompute blit frame cap from new CSS×DPR buffer (same MiB wallet).
        apply_clip_budgets(&self.shared);
        let after = (self.shared.canvas.width(), self.shared.canvas.height());
        if before != after {
            self.repaint_after_buffer_resize()?;
        }
        Ok(())
    }

    /// Restore pixels after the browser wiped the canvas buffer on size change.
    pub(super) fn repaint_after_buffer_resize(&self) -> Result<(), MediaError> {
        let frame = self.shared.player.borrow().state().frame;
        // Prefer live VF when it still matches the playhead (sharp at any size).
        {
            let disp = self.shared.displayed.borrow();
            if let Some(q) = disp.as_ref()
                && q.frame == frame
            {
                return draw_frame_with_context(
                    &self.shared.canvas,
                    &self.shared.ctx,
                    q.video.as_web_sys(),
                );
            }
        }
        // Blit-only Next/scrub may have cleared `displayed` — scale warm ImageBitmap.
        if self.try_present_warm_blit(frame)? {
            return Ok(());
        }
        // Last resort: whatever VF we still hold (better than black).
        let disp = self.shared.displayed.borrow();
        if let Some(q) = disp.as_ref() {
            return draw_frame_with_context(
                &self.shared.canvas,
                &self.shared.ctx,
                q.video.as_web_sys(),
            );
        }
        Ok(())
    }
    pub fn pipeline_stats(&self) -> PipelineStats {
        // Do not hold `presentation` across the dense-cache borrow — park/fill
        // may re-enter on VideoFrame drop and need the queue RefCell.
        let (presentation_len, presentation_from, presentation_to) = {
            let presentation = self.shared.presentation.borrow();
            (
                presentation.len(),
                presentation.front().map(|q| q.frame),
                presentation.back().map(|q| q.frame),
            )
        };
        let (cache_chunks, cache_frames, cache_cap, cache_no_gaps, ready_frames, win_from, win_to) = {
            let cache = self.shared.scrub_chunks.borrow();
            let blit = self.shared.scrub_blit.borrow();
            let extent = cache.extent();
            let mut ready = Vec::new();
            // Solid strip runs only (markers + blit). Prefetch VF rings are meters,
            // not painted on the scrubber (no ahead/behind ticks).
            for (lo, hi) in coalesce_frame_runs(cache.cached_frame_ids()) {
                ready.push(ReadyFrameMark::run(lo, hi, ReadyFrameKind::Covered));
            }
            for (lo, hi) in coalesce_frame_runs(blit.cached_frame_ids()) {
                ready.push(ReadyFrameMark::run(lo, hi, ReadyFrameKind::Exact));
            }
            ready.sort_by(|a, b| {
                a.frame.cmp(&b.frame).then_with(|| {
                    ready_kind_paint_order(a.kind).cmp(&ready_kind_paint_order(b.kind))
                })
            });
            (
                cache.chunk_count(),
                cache.frame_count(),
                cache.max_frames(),
                cache.no_gaps(),
                ready,
                extent.map(|(a, _)| a),
                extent.map(|(_, b)| b),
            )
        };
        // While decode owns the decoder (`None` in the slot), report busy.
        let (held_len, decode_queue_size) = match self.decoder.borrow().as_ref() {
            Some(d) => (d.held_len(), d.browser_decode_queue_size()),
            None => (usize::MAX, u32::MAX),
        };
        PipelineStats {
            presentation_len,
            presentation_cap: self.play_presentation_cap(),
            presentation_from,
            presentation_to,
            held_len,
            decode_queue_size,
            frames_skipped: self.shared.frames_skipped.get(),
            last_behind_ms: self.shared.last_behind_ms.get(),
            underrun_holds: self.shared.underrun_holds.get(),
            underrun_hold_ms: self.underrun_hold_ms_now(),
            cache_chunks,
            cache_frames,
            cache_cap,
            cache_no_gaps,
            scrub_busy: self.shared.scrub_busy.get(),
            scrub_window_from: win_from,
            scrub_window_to: win_to,
            ready_frames,
            gop_profile: self.shared.player.borrow().metadata().gop_profile,
            fetch_file_ranges: self.shared.fetch_map.file_fractions(self.shared.file_size),
        }
    }

    pub fn frame_duration_seconds(&self) -> Result<f64, MediaError> {
        self.duration_of(self.state().frame)
    }

    pub(super) fn duration_of(&self, frame: FrameId) -> Result<f64, MediaError> {
        let player = self.shared.player.borrow();
        let index = player.track().index();
        let entry = index.get(frame).ok_or(MediaError::FrameOutOfRange(
            frame,
            player.state().frame_count,
        ))?;
        if entry.duration_ticks == 0 || entry.timescale == 0 {
            return Err(MediaError::MissingFrameDuration(frame));
        }
        Ok(entry.duration_ticks as f64 / entry.timescale as f64)
    }

    pub(super) fn frame_timestamp_seconds(&self, frame: FrameId) -> Result<f64, MediaError> {
        let player = self.shared.player.borrow();
        let index = player.track().index();
        let entry = index.get(frame).ok_or(MediaError::FrameOutOfRange(
            frame,
            player.state().frame_count,
        ))?;
        Ok(entry.composition_timestamp().seconds)
    }

    pub(super) fn clear_presentation(&self) {
        let mut presentation = self.shared.presentation.borrow_mut();
        while let Some(q) = presentation.pop_front() {
            drop(q.video);
        }
    }

    /// Drop live step VF ring (displayed + behind). Seek / scrub settle / teardown.
    pub(super) fn clear_step_ring(&self) {
        let displayed = self.shared.displayed.borrow_mut().take();
        if let Some(q) = displayed {
            self.ingest_scrub_frame(q.frame, q.video, true);
        }
        loop {
            let next = self.shared.step_behind.borrow_mut().pop_front();
            let Some(q) = next else {
                break;
            };
            self.ingest_scrub_frame(q.frame, q.video, true);
        }
    }

    pub(super) fn take_presentation_front(&self, frame: FrameId) -> Option<OwnedVideoFrame> {
        let mut presentation = self.shared.presentation.borrow_mut();
        if presentation.front().is_some_and(|f| f.frame == frame) {
            presentation.pop_front().map(|q| q.video)
        } else {
            None
        }
    }

    pub(super) fn push_presentation_front(&self, q: QueuedFrame) {
        let mut presentation = self.shared.presentation.borrow_mut();
        presentation.retain(|x| x.frame != q.frame);
        presentation.push_front(q);
        let cap = self.play_presentation_cap();
        while presentation.len() > cap {
            if let Some(drop_q) = presentation.pop_back() {
                drop(drop_q.video);
            }
        }
    }

    pub(super) fn take_step_behind(&self, frame: FrameId) -> Option<OwnedVideoFrame> {
        let mut behind = self.shared.step_behind.borrow_mut();
        if behind.back().is_some_and(|q| q.frame == frame) {
            return behind.pop_back().map(|q| q.video);
        }
        if let Some(i) = behind.iter().position(|q| q.frame == frame) {
            return behind.remove(i).map(|q| q.video);
        }
        None
    }

    pub(super) fn push_step_behind(&self, q: QueuedFrame) {
        {
            let mut behind = self.shared.step_behind.borrow_mut();
            behind.retain(|x| x.frame != q.frame);
            // Keep presentation order (oldest at front).
            match behind.iter().position(|x| x.frame > q.frame) {
                Some(i) => behind.insert(i, q),
                None => behind.push_back(q),
            }
        }
        self.trim_step_behind();
    }

    pub(super) fn step_behind_cap(&self) -> usize {
        if self.shared.engine.get().is_play() {
            STEP_BEHIND_CAP_PLAY
        } else {
            STEP_BEHIND_CAP
        }
    }

    /// Drop all step-behind VFs (Play start — free decoder pool).
    pub(super) fn clear_step_behind_ring(&self) {
        let drained: Vec<_> = self.shared.step_behind.borrow_mut().drain(..).collect();
        for old in drained {
            drop(old.video);
        }
    }

    pub(super) fn trim_step_behind(&self) {
        let cap = self.step_behind_cap();
        loop {
            let overflow = self.shared.step_behind.borrow().len() > cap;
            if !overflow {
                break;
            }
            let old = self.shared.step_behind.borrow_mut().pop_front();
            let Some(old) = old else {
                break;
            };
            // Play: drop aged VF — densify (Pause) owns scrub residency.
            if self.is_playing() {
                drop(old.video);
            } else {
                self.ingest_scrub_frame(old.frame, old.video, true);
            }
        }
    }

    /// Draw `video`, keep it as displayed; retire prior displayed into behind.
    pub(super) fn install_displayed(
        &self,
        frame: FrameId,
        video: OwnedVideoFrame,
    ) -> Result<(), MediaError> {
        let prev = self.shared.displayed.borrow_mut().take();
        if let Some(old) = prev {
            if self.shared.engine.get().is_play() {
                // Play: close immediately — do not pin behind VFs (pool pressure).
                drop(old.video);
            } else {
                self.push_step_behind(old);
            }
        }
        self.install_displayed_no_retire(frame, video)
    }

    pub(super) fn install_displayed_no_retire(
        &self,
        frame: FrameId,
        video: OwnedVideoFrame,
    ) -> Result<(), MediaError> {
        let result =
            draw_frame_with_context(&self.shared.canvas, &self.shared.ctx, video.as_web_sys());
        *self.shared.displayed.borrow_mut() = Some(QueuedFrame { frame, video });
        result
    }

    /// Best-effort Exact residency for the live displayed VF (bitmap only; keep VF).
    pub(super) fn exact_ingest_displayed_borrow(&self) {
        let Ok(disp) = self.shared.displayed.try_borrow() else {
            return;
        };
        let Some(q) = disp.as_ref() else {
            return;
        };
        let target_w = self.shared.canvas.width().max(1);
        let target_h = self.shared.canvas.height().max(1);
        let _ = ingest_scrub_bitmap_only(
            &self.shared,
            q.frame,
            q.video.as_web_sys(),
            target_w,
            target_h,
            true,
        );
    }

    /// Draw and remember a visited frame in the step VF ring + scrub bitmap store.
    ///
    /// Keeps a live `VideoFrame` in `displayed` for Prev; densify mark via bitmap.
    pub(super) fn present_and_cache(
        &self,
        frame: FrameId,
        video: OwnedVideoFrame,
    ) -> Result<(), MediaError> {
        self.install_displayed(frame, video)?;
        self.exact_ingest_displayed_borrow();
        Ok(())
    }

    /// Play present: draw + keep VF in ring; Exact ingest when frames age out of behind.
    pub(super) fn present_play_frame(
        &self,
        frame: FrameId,
        video: OwnedVideoFrame,
    ) -> Result<(), MediaError> {
        self.install_displayed(frame, video)?;
        Ok(())
    }

    pub(super) fn key_for(&self, frame: FrameId) -> Option<FrameId> {
        self.shared
            .player
            .borrow()
            .track()
            .index()
            .nearest_keyframe(frame)
    }

    pub(super) fn touch_playhead(&self, frame: FrameId) {
        if let Ok(mut cache) = self.shared.scrub_chunks.try_borrow_mut() {
            cache.touch_playhead(frame);
        }
        if let Ok(mut blit) = self.shared.scrub_blit.try_borrow_mut() {
            blit.set_playhead(frame);
        }
    }

    pub(super) fn clear_loop_prime(&self) {
        let taken = self.shared.loop_prime.borrow_mut().take();
        if let Some(buf) = taken {
            for q in buf {
                drop(q.video);
            }
        }
    }

    pub(super) fn present_for_mode(
        &self,
        frame: FrameId,
        video: OwnedVideoFrame,
    ) -> Result<(), MediaError> {
        if self.is_playing() && self.shared.engine.get().is_play() {
            self.present_play_frame(frame, video)
        } else {
            self.present_and_cache(frame, video)
        }
    }

    /// Draw without inserting (scrub preview — presentation queue owns ahead frames).
    pub(super) fn present_only(&self, video: OwnedVideoFrame) -> Result<(), MediaError> {
        let result =
            draw_frame_with_context(&self.shared.canvas, &self.shared.ctx, video.as_web_sys());
        drop(video);
        result
    }
}
