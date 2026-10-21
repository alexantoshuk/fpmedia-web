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
        let frame = self.shared.frame.get();
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
        // Play keeps pixels in the canvas pool, not a live VF.
        if self.repaint_from_play_pool(frame)? {
            return Ok(());
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
    /// Snapshot pipeline planes requested by [`PipelineInterest`] (strips / debug UI).
    ///
    /// Unrequested planes stay at [`PipelineStats`] defaults — no ready coalesce /
    /// FetchMap snap / meter reads.
    pub fn pipeline_stats(&self, want: super::PipelineInterest) -> PipelineStats {
        if !want.any() {
            return PipelineStats::default();
        }

        let mut out = PipelineStats::default();

        if want.meters {
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
            let (held_len, decode_queue_size) = match self.play_iter_stats() {
                Some(stats) => stats,
                None => match self.decoder.borrow().as_ref() {
                    Some(d) => (d.held_len(), d.browser_decode_queue_size()),
                    None => (usize::MAX, u32::MAX),
                },
            };
            if self.play_session_live() {
                // Play: mediabunny `nextFrame` (one pool canvas waiting).
                let next = self.shared.play_next.borrow().as_ref().map(|n| n.frame);
                out.presentation_len = usize::from(next.is_some());
                out.presentation_cap = 1;
                out.presentation_from = next;
                out.presentation_to = next;
            } else {
                out.presentation_len = presentation_len;
                out.presentation_cap = self.play_presentation_cap();
                out.presentation_from = presentation_from;
                out.presentation_to = presentation_to;
            }
            out.held_len = held_len;
            out.decode_queue_size = decode_queue_size;
            out.play_waits = self.shared.play_iter.borrow().as_ref().map(|it| it.waits());
            out.frames_skipped = self.shared.frames_skipped.get();
            out.last_behind_ms = self.shared.last_behind_ms.get();
            out.underrun_holds = self.shared.underrun_holds.get();
            out.underrun_hold_ms = self.underrun_hold_ms_now();
            out.scrub_busy = self.shared.densify.is_busy();
            out.gop_profile = self.shared.track.metadata_ref().gop_profile;
        }

        if want.ready || want.meters {
            let cache = self.shared.scrub_chunks.borrow();
            let blit = self.shared.scrub_blit.borrow();
            if want.meters {
                let extent = cache.extent();
                out.cache_chunks = cache.chunk_count();
                out.cache_frames = cache.frame_count();
                out.cache_cap = cache.max_frames();
                out.cache_no_gaps = cache.no_gaps();
                out.scrub_window_from = extent.map(|(a, _)| a);
                out.scrub_window_to = extent.map(|(_, b)| b);
            }
            if want.ready {
                let mut ready = Vec::new();
                // Structural sync ticks (GOP starts) — not densify residency.
                for id in self.shared.track.index().sync_frame_ids() {
                    ready.push(ReadyFrameMark::run(id, id, ReadyFrameKind::Keyframes));
                }
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
                out.ready_frames = ready;
            }
        }

        if want.fetch {
            out.fetch_file_ranges = self.shared.fetch_map.file_fractions(self.shared.file_size);
        }

        out
    }

    pub fn frame_duration_seconds(&self) -> Result<f64, MediaError> {
        self.duration_of(self.state().frame)
    }

    pub(super) fn duration_of(&self, frame: FrameId) -> Result<f64, MediaError> {
        let index = self.shared.track.index();
        let entry = index.get(frame).ok_or(MediaError::FrameOutOfRange(
            frame,
            self.shared.track.frame_count(),
        ))?;
        if entry.duration_ticks == 0 || entry.timescale == 0 {
            return Err(MediaError::MissingFrameDuration(frame));
        }
        Ok(entry.duration_ticks as f64 / entry.timescale as f64)
    }

    pub(super) fn frame_timestamp_seconds(&self, frame: FrameId) -> Result<f64, MediaError> {
        let index = self.shared.track.index();
        let entry = index.get(frame).ok_or(MediaError::FrameOutOfRange(
            frame,
            self.shared.track.frame_count(),
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

    pub(super) fn key_for(&self, frame: FrameId) -> Option<FrameId> {
        self.shared.track.index().nearest_keyframe(frame)
    }

    /// True when a painted hover preview is the same sync key as `aim`.
    ///
    /// Far interim (other GOP) must still schedule aim decode — otherwise a
    /// single warm thumb short-circuits Play refine forever (no gray / no update).
    pub fn hover_thumb_close_enough(&self, aim: FrameId, painted: FrameId) -> bool {
        let ka = self.key_for(aim).unwrap_or(aim);
        let kp = self.key_for(painted).unwrap_or(painted);
        ka == kp
    }

    pub(super) fn touch_playhead(&self, frame: FrameId) {
        if let Ok(mut cache) = self.shared.scrub_chunks.try_borrow_mut() {
            cache.touch_playhead(frame);
        }
        if let Ok(mut blit) = self.shared.scrub_blit.try_borrow_mut() {
            blit.set_playhead(frame);
        }
    }
}
