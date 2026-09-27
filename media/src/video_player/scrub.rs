//! Scrub densify / hover-thumb orchestration for [`super::VideoPlayer`].

use std::rc::Rc;

use web_sys::HtmlCanvasElement;

use crate::decode::owned_video_frame::{AsVideoFrame, OwnedVideoFrame};
use crate::decode::scrub_blit::{ScrubChunk, draw_image_bitmap};
#[cfg(feature = "thumbs")]
use crate::decode::thumb_preview::{ThumbPreview, paint_thumb_canvas, run_sparse_key_fill};
use crate::decode::webcodecs::WebCodecsDecoder;
use crate::engine_mode::{EngineEvent, reduce as engine_reduce};
use crate::error::MediaError;
use crate::types::{
    FrameId, GopProfile, ScrubPolicy, ThumbPolicy, exact_frames_to_free_for_play_vram,
    hover_thumb_dims, scrub_blit_cap_during_play, scrub_blit_frame_budget, scrub_preview_choice,
};

use super::{
    PLAY_EXACT_VRAM_VF_SLACK, Shared, VideoPlayer, performance_now_ms, sleep_ms,
    wait_animation_frame, yield_event_loop,
};

impl VideoPlayer {
    /// Abort in-flight hover Exact downscale (scrub start / pointer leave).
    /// Do **not** call from [`Self::play`] — thumbs stay across Play
    /// (`mediabunny:` separate thumb `CanvasSink`).
    pub fn cancel_hover_thumb(&self) {
        #[cfg(feature = "thumbs")]
        if let Ok(slot) = self.shared.thumb.try_borrow()
            && let Some(preview) = slot.as_ref()
        {
            preview.cancel();
        }
    }

    /// Runtime thumb gate. `Off` drops the lazy [`ThumbPreview`] sink.
    #[cfg(feature = "thumbs")]
    pub fn set_thumb_policy(&self, policy: ThumbPolicy) {
        self.shared.thumb_policy.set(policy);
        if policy == ThumbPolicy::Off {
            self.cancel_hover_thumb();
            if let Ok(mut slot) = self.shared.thumb.try_borrow_mut() {
                let _ = slot.take();
            }
        }
    }

    #[cfg(feature = "thumbs")]
    pub fn thumb_policy(&self) -> ThumbPolicy {
        self.shared.thumb_policy.get()
    }

    /// Soft stub when feature `thumbs` is off.
    #[cfg(not(feature = "thumbs"))]
    pub fn set_thumb_policy(&self, _policy: ThumbPolicy) {}
    #[cfg(not(feature = "thumbs"))]
    pub fn thumb_policy(&self) -> ThumbPolicy {
        ThumbPolicy::Off
    }

    /// Ensure a [`ThumbPreview`] exists when policy is `On`. Returns `false` if off / unavailable.
    #[cfg(feature = "thumbs")]
    pub(super) fn ensure_thumb_preview(&self) -> bool {
        if self.shared.thumb_policy.get() != ThumbPolicy::On {
            return false;
        }
        let Ok(mut slot) = self.shared.thumb.try_borrow_mut() else {
            return false;
        };
        if slot.is_none() {
            *slot = Some(Rc::new(ThumbPreview::new()));
        }
        true
    }

    /// Sync hover paint from [`ThumbPreview`] / nearest available preview.
    ///
    /// Picks the closer of ThumbCache-nearest and Exact-nearest via
    /// [`scrub_preview_choice`] (same soft-drag rule as the scrubber). When
    /// Exact wins, downscales into ThumbCache **synchronously** (cheap
    /// `bitmap_to_sized`) so pointermove never leaves a farther thumb on the
    /// canvas. `mediabunny:` present cached canvas immediately; decode only on
    /// true miss. Allowed while playing.
    pub fn try_paint_hover_thumb_cached(
        &self,
        frame: FrameId,
        dest: &HtmlCanvasElement,
        max_w: u32,
        max_h: u32,
    ) -> Result<Option<FrameId>, MediaError> {
        #[cfg(not(feature = "thumbs"))]
        {
            let _ = (frame, dest, max_w, max_h);
            return Err(MediaError::Cancelled);
        }
        #[cfg(feature = "thumbs")]
        {
            if !self.ensure_thumb_preview() {
                return Err(MediaError::Cancelled);
            }
            let count = self.state().frame_count;
            if count == 0 {
                return Err(MediaError::FrameOutOfRange(frame, count));
            }
            let frame = frame.min(count.saturating_sub(1));
            if let Ok(slot) = self.shared.thumb.try_borrow()
                && let Some(thumbs) = slot.as_ref()
            {
                thumbs.note_size(max_w, max_h);
                thumbs.set_hover_aim(frame);
            }
            let meta = self.shared.player.borrow().metadata().clone();
            let (tw, th) = hover_thumb_dims(meta.width, meta.height, max_w, max_h);

            let cached_exact = self
                .shared
                .scrub_blit
                .try_borrow()
                .ok()
                .and_then(|b| b.nearest_frame(frame));
            let cached_thumb = {
                let Ok(slot) = self.shared.thumb.try_borrow() else {
                    return Ok(None);
                };
                let Some(thumbs) = slot.as_ref() else {
                    return Ok(None);
                };
                thumbs.nearest_frame(frame, tw, th)
            };
            let Some(preview) = scrub_preview_choice(frame, cached_thumb, cached_exact) else {
                return Ok(None);
            };

            // Prefer already-sized ThumbCache when it holds the winner.
            let painted = {
                let Ok(slot) = self.shared.thumb.try_borrow() else {
                    return Ok(None);
                };
                let Some(thumbs) = slot.as_ref() else {
                    return Ok(None);
                };
                let store_key = self.key_for(preview).unwrap_or(preview);
                thumbs.with_bitmap(store_key, preview, tw, th, |bmp| {
                    paint_thumb_canvas(dest, bmp, tw, th)
                })
            };
            let result = match painted {
                Some(Ok(())) => Ok(Some(preview)),
                Some(Err(e)) => Err(e),
                None if cached_exact == Some(preview) => {
                    // Exact winner — sync present (do not leave a farther thumb).
                    match self.try_hover_thumb_from_exact(frame, dest, max_w, max_h)? {
                        Some(id) => Ok(Some(id)),
                        None => {
                            let Ok(slot) = self.shared.thumb.try_borrow() else {
                                return Ok(None);
                            };
                            let Some(thumbs) = slot.as_ref() else {
                                return Ok(None);
                            };
                            match thumbs.with_nearest_bitmap(frame, tw, th, |bmp| {
                                paint_thumb_canvas(dest, bmp, tw, th)
                            }) {
                                Some((id, Ok(()))) => Ok(Some(id)),
                                Some((_, Err(e))) => Err(e),
                                None => Ok(None),
                            }
                        }
                    }
                }
                None => {
                    // ThumbCache reported nearest but get missed (race) — try scan paint.
                    let Ok(slot) = self.shared.thumb.try_borrow() else {
                        return Ok(None);
                    };
                    let Some(thumbs) = slot.as_ref() else {
                        return Ok(None);
                    };
                    match thumbs.with_nearest_bitmap(frame, tw, th, |bmp| {
                        paint_thumb_canvas(dest, bmp, tw, th)
                    }) {
                        Some((id, Ok(()))) => Ok(Some(id)),
                        Some((_, Err(e))) => Err(e),
                        None => Ok(None),
                    }
                }
            };
            if matches!(&result, Ok(Some(_))) {
                self.schedule_thumb_key_fill();
            }
            result
        }
    }

    /// Paint a hover preview into `dest`.
    ///
    /// Sync nearest (ThumbCache ∪ Exact) first; missing sync keys warm via bg
    /// [`Self::schedule_thumb_key_fill`] only — do **not** block the canvas on
    /// key decode (that left a far interim while Exact was nearer).
    /// True cold miss → `create_thumb` decode of aim. `mediabunny:` present
    /// cached canvas immediately; sparse key fill off the present path.
    pub async fn paint_hover_thumb(
        &self,
        frame: FrameId,
        dest: &HtmlCanvasElement,
        max_w: u32,
        max_h: u32,
    ) -> Result<FrameId, MediaError> {
        #[cfg(not(feature = "thumbs"))]
        {
            let _ = (frame, dest, max_w, max_h);
            return Err(MediaError::Cancelled);
        }
        #[cfg(feature = "thumbs")]
        {
            if !self.ensure_thumb_preview() {
                return Err(MediaError::Cancelled);
            }
            let count = self.state().frame_count;
            if count == 0 {
                return Err(MediaError::FrameOutOfRange(frame, count));
            }
            let frame = frame.min(count.saturating_sub(1));
            if let Ok(slot) = self.shared.thumb.try_borrow()
                && let Some(thumbs) = slot.as_ref()
            {
                thumbs.note_size(max_w, max_h);
                thumbs.set_hover_aim(frame);
            }

            let interim = self.try_paint_hover_thumb_cached(frame, dest, max_w, max_h)?;
            self.schedule_thumb_key_fill();
            if let Some(p) = interim {
                return Ok(p);
            }

            // Cold: no ThumbCache / Exact — decode aim (latest-wins).
            self.decode_hover_thumb(frame, dest, max_w, max_h).await
        }
    }

    /// Exact blit → sized thumb into [`ThumbPreview`] (read-only densify plane).
    ///
    /// Only runs when Exact is the [`scrub_preview_choice`] winner vs ThumbCache
    /// (caller already tried cache). Still localizes to Exact-nearest of aim.
    #[cfg(feature = "thumbs")]
    pub(super) fn try_hover_thumb_from_exact(
        &self,
        frame: FrameId,
        dest: &HtmlCanvasElement,
        max_w: u32,
        max_h: u32,
    ) -> Result<Option<FrameId>, MediaError> {
        let cached_exact = self
            .shared
            .scrub_blit
            .try_borrow()
            .ok()
            .and_then(|b| b.nearest_frame(frame));
        let Some(preview) = scrub_preview_choice(frame, None, cached_exact) else {
            return Ok(None);
        };
        let store_key = self.key_for(preview).unwrap_or(preview);
        let meta = self.shared.player.borrow().metadata().clone();
        let (tw, th) = hover_thumb_dims(meta.width, meta.height, max_w, max_h);
        let epoch = {
            let Ok(slot) = self.shared.thumb.try_borrow() else {
                return Ok(None);
            };
            slot.as_ref()
                .map(|t| t.generation())
                .ok_or(MediaError::Cancelled)?
        };
        let cancelled = || {
            self.shared
                .thumb
                .try_borrow()
                .ok()
                .and_then(|s| s.as_ref().map(|t| t.generation() != epoch))
                .unwrap_or(true)
        };
        if cancelled() {
            return Err(MediaError::Cancelled);
        }

        let thumb_from_blit = if let Ok(mut blit) = self.shared.scrub_blit.try_borrow_mut() {
            blit.get(preview).is_some()
        } else {
            false
        };
        if !thumb_from_blit {
            return Ok(None);
        }
        let thumb = {
            let Ok(mut blit) = self.shared.scrub_blit.try_borrow_mut() else {
                return Ok(None);
            };
            let Some(src) = blit.get(preview) else {
                return Ok(None);
            };
            let Ok(slot) = self.shared.thumb.try_borrow() else {
                return Ok(None);
            };
            let Some(thumbs) = slot.as_ref() else {
                return Ok(None);
            };
            thumbs.with_blit(|enc| enc.bitmap_to_sized(src, tw, th))?
        };
        if cancelled() {
            thumb.close();
            return Err(MediaError::Cancelled);
        }
        paint_thumb_canvas(dest, &thumb, tw, th)?;
        if let Ok(slot) = self.shared.thumb.try_borrow()
            && let Some(thumbs) = slot.as_ref()
        {
            thumbs.put_ready(store_key, preview, thumb, tw, th);
        } else {
            thumb.close();
        }
        Ok(Some(preview))
    }

    /// Dedicated 3rd decoder → ThumbCache only (latest-wins coalesce).
    #[cfg(feature = "thumbs")]
    pub(super) async fn decode_hover_thumb(
        &self,
        frame: FrameId,
        dest: &HtmlCanvasElement,
        max_w: u32,
        max_h: u32,
    ) -> Result<FrameId, MediaError> {
        let blocked = {
            let Ok(slot) = self.shared.thumb.try_borrow() else {
                return Err(MediaError::Cancelled);
            };
            slot.as_ref().map(|t| t.decode_blocked()).unwrap_or(true)
        };
        if blocked {
            return Err(MediaError::Cancelled);
        }

        let claim = {
            let Ok(slot) = self.shared.thumb.try_borrow() else {
                return Err(MediaError::Cancelled);
            };
            let Some(thumbs) = slot.as_ref() else {
                return Err(MediaError::Cancelled);
            };
            thumbs.claim_decode(frame)
        };
        if !claim {
            // Bg key-fill owns the worker — wait until free, keep latest aim pending.
            let wait_epoch = {
                let Ok(slot) = self.shared.thumb.try_borrow() else {
                    return Err(MediaError::Cancelled);
                };
                slot.as_ref()
                    .map(|t| t.generation())
                    .ok_or(MediaError::Cancelled)?
            };
            loop {
                yield_event_loop().await;
                let Ok(slot) = self.shared.thumb.try_borrow() else {
                    return Err(MediaError::Cancelled);
                };
                let Some(thumbs) = slot.as_ref() else {
                    return Err(MediaError::Cancelled);
                };
                if thumbs.generation() != wait_epoch || thumbs.decode_blocked() {
                    return Err(MediaError::Cancelled);
                }
                if thumbs.claim_decode(frame) {
                    break;
                }
            }
        }

        let end_decode = || {
            if let Ok(slot) = self.shared.thumb.try_borrow()
                && let Some(thumbs) = slot.as_ref()
            {
                thumbs.end_decode();
            }
        };

        let epoch = {
            let Ok(slot) = self.shared.thumb.try_borrow() else {
                end_decode();
                return Err(MediaError::Cancelled);
            };
            let Some(thumbs) = slot.as_ref() else {
                end_decode();
                return Err(MediaError::Cancelled);
            };
            thumbs.generation()
        };
        let cancelled = || {
            self.shared
                .thumb
                .try_borrow()
                .ok()
                .and_then(|s| s.as_ref().map(|t| t.generation() != epoch))
                .unwrap_or(true)
        };

        let mut last_ok: Option<FrameId> = None;
        loop {
            let aim = {
                let Ok(slot) = self.shared.thumb.try_borrow() else {
                    break;
                };
                let Some(thumbs) = slot.as_ref() else {
                    break;
                };
                thumbs.take_pending_aim().unwrap_or(frame)
            };
            if cancelled() {
                break;
            }
            match self
                .decode_one_hover_thumb(aim, dest, max_w, max_h, &cancelled)
                .await
            {
                Ok(id) => last_ok = Some(id),
                Err(MediaError::Cancelled) => {
                    if cancelled() {
                        break;
                    }
                }
                Err(MediaError::CodecUnsupported(_)) => {
                    if let Ok(slot) = self.shared.thumb.try_borrow()
                        && let Some(thumbs) = slot.as_ref()
                    {
                        thumbs.set_decode_blocked(true);
                    }
                    break;
                }
                Err(_) => {
                    // Keep prior pixels; still drain pending aims.
                }
            }
            let more = self
                .shared
                .thumb
                .try_borrow()
                .ok()
                .and_then(|s| s.as_ref().map(|t| t.has_pending_aim()))
                .unwrap_or(false);
            if !more {
                break;
            }
        }

        end_decode();
        let result = last_ok.ok_or(MediaError::Cancelled);
        self.schedule_thumb_key_fill();
        result
    }

    #[cfg(feature = "thumbs")]
    pub(super) async fn decode_one_hover_thumb(
        &self,
        frame: FrameId,
        dest: &HtmlCanvasElement,
        max_w: u32,
        max_h: u32,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<FrameId, MediaError> {
        if cancelled() {
            return Err(MediaError::Cancelled);
        }

        let meta = self.shared.player.borrow().metadata().clone();
        let (tw, th) = hover_thumb_dims(meta.width, meta.height, max_w, max_h);
        let store_key = self.key_for(frame).unwrap_or(frame);

        // Skip decode only when **this** frame is already in ThumbCache (not a far neighbor).
        let already = self
            .shared
            .thumb
            .try_borrow()
            .ok()
            .and_then(|s| s.as_ref().map(|t| t.contains(store_key, frame, tw, th)))
            .unwrap_or(false);
        if already {
            let paint_aim = self
                .shared
                .thumb
                .try_borrow()
                .ok()
                .and_then(|s| s.as_ref().map(|t| t.hover_aim()))
                .unwrap_or(frame);
            if let Some(hit) = self.try_paint_hover_thumb_cached(paint_aim, dest, max_w, max_h)? {
                return Ok(hit);
            }
            return Ok(frame);
        }

        // Prefer Exact downscale into ThumbCache when Exact holds this frame.
        if self
            .shared
            .scrub_blit
            .try_borrow()
            .ok()
            .is_some_and(|b| b.contains(frame))
            && let Some(preview) = self.try_hover_thumb_from_exact(frame, dest, max_w, max_h)?
            && preview == frame
        {
            return Ok(preview);
        }
        if cancelled() {
            return Err(MediaError::Cancelled);
        }

        let track = self.shared.player.borrow().track_rc();

        let taken = {
            let Ok(slot) = self.shared.thumb.try_borrow() else {
                return Err(MediaError::Cancelled);
            };
            let Some(thumbs) = slot.as_ref() else {
                return Err(MediaError::Cancelled);
            };
            thumbs.take_decoder()
        };
        let mut decoder = match taken {
            Some(d) => d,
            None => match WebCodecsDecoder::create_thumb(&meta).await {
                Ok(d) => d,
                Err(MediaError::CodecUnsupported(c)) => {
                    return Err(MediaError::CodecUnsupported(c));
                }
                Err(e) => return Err(e),
            },
        };
        if cancelled() {
            if let Ok(slot) = self.shared.thumb.try_borrow()
                && let Some(thumbs) = slot.as_ref()
            {
                thumbs.put_decoder(decoder);
            }
            return Err(MediaError::Cancelled);
        }

        let decoded = decoder.decode_frame(&track, frame, cancelled).await;
        let video = match decoded {
            Ok(v) => v,
            Err(e) => {
                let _ = decoder.reset();
                if let Ok(slot) = self.shared.thumb.try_borrow()
                    && let Some(thumbs) = slot.as_ref()
                {
                    thumbs.put_decoder(decoder);
                }
                return Err(e);
            }
        };
        if let Ok(slot) = self.shared.thumb.try_borrow()
            && let Some(thumbs) = slot.as_ref()
        {
            thumbs.put_decoder(decoder);
        }

        if cancelled() {
            drop(video);
            return Err(MediaError::Cancelled);
        }

        let thumb = {
            let Ok(slot) = self.shared.thumb.try_borrow() else {
                drop(video);
                return Err(MediaError::Cancelled);
            };
            let Some(thumbs) = slot.as_ref() else {
                drop(video);
                return Err(MediaError::Cancelled);
            };
            thumbs.with_blit(|enc| {
                enc.frame_to_bitmap(video.as_video_frame(), tw, th)
                    .map(|(bmp, _, _)| bmp)
            })?
        };
        drop(video);
        if cancelled() {
            thumb.close();
            return Err(MediaError::Cancelled);
        }
        let paint_aim = {
            let Ok(slot) = self.shared.thumb.try_borrow() else {
                thumb.close();
                return Err(MediaError::Cancelled);
            };
            let Some(thumbs) = slot.as_ref() else {
                thumb.close();
                return Err(MediaError::Cancelled);
            };
            thumbs.put_ready(store_key, frame, thumb, tw, th);
            thumbs.hover_aim()
        };
        if cancelled() {
            return Err(MediaError::Cancelled);
        }
        match self.try_paint_hover_thumb_cached(paint_aim, dest, max_w, max_h)? {
            Some(id) => Ok(id),
            None => {
                if let Some(preview) =
                    self.try_hover_thumb_from_exact(paint_aim, dest, max_w, max_h)?
                {
                    Ok(preview)
                } else {
                    self.try_paint_hover_thumb_cached(paint_aim, dest, max_w, max_h)?
                        .ok_or(MediaError::Cancelled)
                }
            }
        }
    }

    /// Kick sparse keyframe → ThumbCache fill (same 3rd decoder; hover wins).
    #[cfg(feature = "thumbs")]
    pub(super) fn schedule_thumb_key_fill(&self) {
        if !self.ensure_thumb_preview() {
            return;
        }
        let Some(thumbs) = self.thumb_sink() else {
            return;
        };
        if thumbs.decode_blocked() {
            return;
        }
        if thumbs.is_busy() {
            thumbs.set_fill_pending(true);
            return;
        }
        self.kick_thumb_key_fill();
    }

    #[cfg(feature = "thumbs")]
    pub(super) fn thumb_sink(&self) -> Option<Rc<ThumbPreview>> {
        self.shared.thumb.try_borrow().ok().and_then(|s| s.clone())
    }

    #[cfg(feature = "thumbs")]
    pub(super) fn kick_thumb_key_fill(&self) {
        let Some(key) = self.next_thumb_key_gap() else {
            return;
        };
        let Some(thumbs) = self.thumb_sink() else {
            return;
        };
        if !thumbs.claim_fill_key(key) {
            return;
        }
        let shared = Rc::clone(&self.shared);
        let sink = Rc::clone(&thumbs);
        wasm_bindgen_futures::spawn_local(async move {
            let track = shared.player.borrow().track_rc();
            let meta = shared.player.borrow().metadata().clone();
            let next_gap = || next_thumb_key_gap_shared(&shared);
            let _ = run_sparse_key_fill(sink, track, meta, next_gap).await;
        });
    }

    /// Next sync frame missing from ThumbCache, nearest to hover aim / playhead.
    #[cfg(feature = "thumbs")]
    pub(super) fn next_thumb_key_gap(&self) -> Option<FrameId> {
        next_thumb_key_gap_shared(&self.shared)
    }

    /// Soft stub when feature `thumbs` is off.
    #[cfg(not(feature = "thumbs"))]
    pub(super) fn schedule_thumb_key_fill(&self) {}
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
    /// is deferred so exact `goto_frame` owns HW; soft-drag scrub keeps densify on.
    pub fn enter_scrub_cache_pipeline(&self, focus: FrameId) {
        self.aim_scrub_cache(focus);
        if self.scrub_policy().densify {
            self.schedule_scrub_cache_fill();
        }
        #[cfg(feature = "thumbs")]
        self.schedule_thumb_key_fill();
    }

    pub(super) fn scrub_policy(&self) -> ScrubPolicy {
        self.shared.player.borrow().metadata().scrub_policy()
    }

    pub(super) fn is_all_intra(&self) -> bool {
        matches!(
            self.shared.player.borrow().metadata().gop_profile,
            GopProfile::AllIntra
        )
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
        let scrub = self.shared.scrub_epoch.get().wrapping_add(1);
        self.shared.scrub_epoch.set(scrub);
        self.shared.scrub_pending.set(rearm);
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
    /// Exact/seek must not start while `decode_gop` still feeds the scrub
    /// decoder (`mediabunny: seekToTime` → `startVideoIterator` terminates the
    /// prior iterator immediately — no densify plane). Local dual-decoder: brief
    /// yield then Exact; long URL Range stalls must not block settle for seconds.
    /// Poison recovery: Exact soft-cold / recreate + PlayPrime fill recreate.
    /// If still busy after the first window, bump abort again and drain once more.
    pub(super) async fn await_scrub_densify_yield(&self) {
        const BUDGET_MS: f64 = 250.0;
        const RETRY_MS: f64 = 250.0;
        let start = performance_now_ms().unwrap_or(0.0);
        while self.shared.scrub_busy.get() {
            let now = performance_now_ms().unwrap_or(start);
            if now - start >= BUDGET_MS {
                break;
            }
            sleep_ms(0).await;
        }
        if !self.shared.scrub_busy.get() {
            return;
        }
        web_sys::console::warn_1(&"webmedia: densify still busy after yield — re-abort".into());
        self.abort_scrub_fill(true);
        let start2 = performance_now_ms().unwrap_or(0.0);
        while self.shared.scrub_busy.get() {
            let now = performance_now_ms().unwrap_or(start2);
            if now - start2 >= RETRY_MS {
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
        self.abort_scrub_fill(true);
        let (next, _) = engine_reduce(self.shared.engine.get(), EngineEvent::BeginExact);
        self.shared.engine.set(next);
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

    /// Drag sample entry — clear ExactHold so densify can run under the finger.
    ///
    /// `prepare_scrub_settle` may have latched hold; if settle is superseded by a
    /// new drag before `scrub_to` clears it, densify stayed dead until Pause.
    pub fn clear_densify_hold(&self) {
        if !self.shared.engine.get().holds_exact() {
            return;
        }
        let (next, _) = engine_reduce(self.shared.engine.get(), EngineEvent::EndExact);
        self.shared.engine.set(next);
        self.schedule_scrub_cache_fill();
    }

    /// Scrub pointer sample (drag / hold). Same cache pipeline as pause + release.
    pub fn note_scrub_frame(&self, frame: FrameId) {
        self.cancel_hover_thumb();
        self.note_scrub_direction(frame);
        self.enter_scrub_cache_pipeline(frame);
    }

    /// Async timeline scrub (local scrub policy; settle always exact VF).
    ///
    /// - **Drag** (`settle=false`): densify while moving. [`ScrubDragMode::Soft`]
    ///   (short-IP / All-Intra) paints warm blit only — no decode on the OpQueue.
    ///   Exact drag (Long) may soft-decode preview then `goto_frame`.
    /// - **Settle** (`settle=true`, pointer up): abort densify, then one key→exact
    ///   cascade (`goto_frame_with_interim`) so canvas gets decode `VideoFrame`
    ///   (not a soft still). Interim preview may blit cache; pruned frames fill
    ///   densify markers without blit.
    pub async fn scrub_to(&self, frame: FrameId, settle: bool) -> Result<(), MediaError> {
        let epoch = self.begin_op();
        let soft_drag = self.scrub_policy().soft_drag();
        let densify_yield_ms = if settle {
            // Host may already have called [`Self::prepare_scrub_settle`]; reinforce.
            self.abort_scrub_fill(true);
            let yield_t0 = performance_now_ms().unwrap_or(0.0);
            self.await_scrub_densify_yield().await;
            let yield_ms = performance_now_ms().unwrap_or(yield_t0) - yield_t0;
            let (next, _) = engine_reduce(self.shared.engine.get(), EngineEvent::BeginExact);
            self.shared.engine.set(next);
            self.clear_scrub_direction();
            self.apply_session_event(crate::SessionEvent::BeginRestart);
            self.aim_scrub_cache(frame);
            yield_ms
        } else {
            // Drag: ExactHold → Scrub so densify can run under the finger.
            let mut engine = self.shared.engine.get();
            engine.mode = crate::EngineMode::Scrub;
            self.shared.engine.set(engine);
            self.apply_session_event(crate::SessionEvent::BeginRestart);
            self.note_scrub_direction(frame);
            // Drag: kick densify immediately so the strip fills while the finger moves.
            self.enter_scrub_cache_pipeline(frame);
            0.0
        };
        let result = async {
            self.shared.player.borrow_mut().pause();
            self.clear_loop_prime();

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
                if !exact_scrub_ready(&self.shared, frame) {
                    // Soft-present immediately, then briefly poll for GOP bytes and
                    // Exact if they land — play→scrub→resume otherwise soft-colds
                    // with empty seed and PlayPrime cold-fills / seek-hangs.
                    // Do **not** `warm_sample_bytes` here (can stall seconds on URL).
                    // mediabunny: startVideoIterator awaits first packets before play.
                    soft_cold_scrub_settle(self, frame)?;
                    self.shared.pending_cold_exact.set(Some(frame));
                    let warm_t0 = performance_now_ms().unwrap_or(0.0);
                    const WARM_EXACT_POLL_MS: f64 = 400.0;
                    let key = self.key_for(frame).unwrap_or(frame);
                    warm_scrub_gop_bytes(&self.shared, key);
                    while !exact_scrub_ready(&self.shared, frame) {
                        let now = performance_now_ms().unwrap_or(warm_t0);
                        if now - warm_t0 >= WARM_EXACT_POLL_MS {
                            break;
                        }
                        if !self.is_current(epoch) {
                            return Err(MediaError::Cancelled);
                        }
                        sleep_ms(0).await;
                    }
                    let warm_ms = performance_now_ms().unwrap_or(warm_t0) - warm_t0;
                    if exact_scrub_ready(&self.shared, frame) {
                        self.shared.pending_cold_exact.set(None);
                        let exact_t0 = performance_now_ms().unwrap_or(0.0);
                        match self.exact_goto_with_decoder_retry(frame, epoch).await {
                            Ok(()) => {
                                let exact_ms =
                                    performance_now_ms().unwrap_or(exact_t0) - exact_t0;
                                let seed_t0 = performance_now_ms().unwrap_or(0.0);
                                match self.seed_play_presentation_after_seek(epoch).await {
                                    Ok(()) => {
                                        let seed_ms = performance_now_ms().unwrap_or(seed_t0)
                                            - seed_t0;
                                        web_sys::console::log_1(
                                            &format!(
                                                "webmedia: settle Exact after warm densify_yield={densify_yield_ms:.0}ms warm={warm_ms:.0}ms exact={exact_ms:.0}ms seed={seed_ms:.0}ms"
                                            )
                                            .into(),
                                        );
                                    }
                                    Err(e) if e.is_cancelled() => return Err(e),
                                    Err(e) if super::is_decoder_poison(&e) => {
                                        web_sys::console::warn_1(
                                            &format!("webmedia: Exact seed poison after warm → keep soft cold ({e})").into(),
                                        );
                                        soft_cold_scrub_settle(self, frame)?;
                                        self.shared.pending_cold_exact.set(Some(frame));
                                    }
                                    Err(e) => return Err(e),
                                }
                            }
                            Err(e) if e.is_cancelled() => return Err(e),
                            Err(e) if super::is_decoder_poison(&e) => {
                                web_sys::console::warn_1(
                                    &format!("webmedia: Exact poison after warm → soft cold ({e})").into(),
                                );
                                soft_cold_scrub_settle(self, frame)?;
                                self.shared.pending_cold_exact.set(Some(frame));
                            }
                            Err(e) => return Err(e),
                        }
                    } else {
                        web_sys::console::log_1(
                            &format!(
                                "webmedia: settle soft-cold (densify_yield={densify_yield_ms:.0}ms warm_poll={warm_ms:.0}ms)"
                            )
                            .into(),
                        );
                    }
                } else {
                    self.shared.pending_cold_exact.set(None);
                    let exact_t0 = performance_now_ms().unwrap_or(0.0);
                    match self.exact_goto_with_decoder_retry(frame, epoch).await {
                        Ok(()) => {
                            let exact_ms =
                                performance_now_ms().unwrap_or(exact_t0) - exact_t0;
                            // Always seed first play frames after Exact (mediabunny
                            // `seekToTime` → `startVideoIterator` awaits two canvases
                            // before optional `play`). Soft-drag / densify unchanged.
                            // PlayPrime then skips byte-warm + fill when queue is primed.
                            // Seed poison must not sticky-UI after a painted Exact —
                            // fall back like Exact poison (cold-tap / backpressure race).
                            let seed_t0 = performance_now_ms().unwrap_or(0.0);
                            match self.seed_play_presentation_after_seek(epoch).await {
                                Ok(()) => {
                                    let seed_ms =
                                        performance_now_ms().unwrap_or(seed_t0) - seed_t0;
                                    web_sys::console::log_1(
                                        &format!(
                                            "webmedia: settle Exact densify_yield={densify_yield_ms:.0}ms exact={exact_ms:.0}ms seed={seed_ms:.0}ms"
                                        )
                                        .into(),
                                    );
                                }
                                Err(e) if e.is_cancelled() => return Err(e),
                                Err(e) if super::is_decoder_poison(&e) => {
                                    let seed_ms =
                                        performance_now_ms().unwrap_or(seed_t0) - seed_t0;
                                    web_sys::console::warn_1(
                                        &format!("webmedia: Exact seed poison → soft cold ({e}); densify_yield={densify_yield_ms:.0}ms exact={exact_ms:.0}ms seed={seed_ms:.0}ms")
                                            .into(),
                                    );
                                    soft_cold_scrub_settle(self, frame)?;
                                    self.shared.pending_cold_exact.set(Some(frame));
                                }
                                Err(e) => return Err(e),
                            }
                        }
                        Err(e) if e.is_cancelled() => return Err(e),
                        Err(e) if super::is_decoder_poison(&e) => {
                            let exact_ms =
                                performance_now_ms().unwrap_or(exact_t0) - exact_t0;
                            // Retry already failed — soft present + densify later
                            // instead of sticky UI `Decoding error` (cold-tap race).
                            web_sys::console::warn_1(
                                &format!("webmedia: Exact poison → soft cold settle ({e}); densify_yield={densify_yield_ms:.0}ms exact={exact_ms:.0}ms").into(),
                            );
                            soft_cold_scrub_settle(self, frame)?;
                            self.shared.pending_cold_exact.set(Some(frame));
                        }
                        Err(e) => return Err(e),
                    }
                }
                Ok(())
            } else {
                if self.present_from_cache(frame).await? {
                    self.shared.player.borrow_mut().commit_frame(frame)?;
                    return Ok(());
                }

                if soft_drag {
                    // Soft-drag: warm blit only (aim or nearer preview). Never
                    // `decode_frame` here — that blocked OpQueue on URL (HEVC
                    // key→N per pointer sample) while densify already fills Exact.
                    // Settle does exact VF. mediabunny: CanvasSink pool present;
                    // cascade on settle / samplesAtTimestamps, not every drag tick.
                    if let Some(preview) = self.scrub_preview_frame(frame) {
                        let _ = self.try_present_warm_blit(preview)?;
                    }
                    return Ok(());
                }

                // Exact-drag (Long): may paint key/neighbor via decode, then goto.
                if let Some(preview) = self.scrub_preview_frame(frame)
                    && preview != frame
                {
                    self.draw_frame_unccommitted(preview, epoch).await?;
                    if !self.is_current(epoch) {
                        return Err(MediaError::Cancelled);
                    }
                    if self.present_from_cache(frame).await? {
                        self.shared.player.borrow_mut().commit_frame(frame)?;
                        return Ok(());
                    }
                }
                self.goto_frame(frame, epoch).await
            }
        }
        .await;
        self.apply_session_event(crate::SessionEvent::EndRestart);
        // End ExactHold after settle/drag op (densify arm is host-gated on settle).
        let (next, _) = engine_reduce(self.shared.engine.get(), EngineEvent::EndExact);
        self.shared.engine.set(next);
        // Cancelled (superseded drag / seek): do not kick densify — a following
        // PlayPrime must keep a warm main decoder when possible.
        if !self.is_current(epoch) {
            return Err(MediaError::Cancelled);
        }
        if settle {
            // Densify is armed by the host *after* the drain loop can see a queued
            // PlayPrime — otherwise invalidate + scrub HW kick cause play-after-scrub
            // lag on long GOPs (long-GOP). Drag still densifies below.
            // Do **not** `schedule_scrub_cache_fill` here when Play is pending — host
            // skips `arm_scrub_densify_after_exact` on purpose; Pause restarts fill.
            // Cold soft-settle: kick densify so Exact/`pending_cold_exact` can resolve
            // when bytes arrive (host may skip arm after settle).
            if self.shared.pending_cold_exact.get().is_some() {
                self.enter_scrub_cache_pipeline(frame);
            }
        } else {
            self.enter_scrub_cache_pipeline(frame);
        }
        result?;
        self.snap_media_clock_to_playhead();
        Ok(())
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

    /// Best interim canvas frame while exact `target` is missing from dense cache.
    ///
    /// Prefer a cached neighbor when `|cached − target| ≤ |key − target|` (tie →
    /// cache: already decoded, no key jump). Otherwise the nearest sync key.
    pub(super) fn scrub_preview_frame(&self, target: FrameId) -> Option<FrameId> {
        let key = self.key_for(target);
        let cached = self.shared.scrub_chunks.borrow().nearest_frame(target);
        crate::scrub_preview_choice(target, key, cached)
    }

    /// Decode/draw `frame` for canvas preview without updating the committed playhead.
    ///
    /// Cache hits draw in place. Key (uncached) previews decode without inserting
    /// into dense — orphan syncs must not punch holes outside the scrub window.
    pub(super) async fn draw_frame_unccommitted(
        &self,
        frame: FrameId,
        epoch: u64,
    ) -> Result<(), MediaError> {
        if self.present_from_cache(frame).await? {
            return Ok(());
        }
        let track = self.shared.player.borrow().track_rc();
        let video_frame = self.decode_frame(&track, frame, epoch).await?;
        if !self.is_current(epoch) {
            drop(video_frame);
            return Err(MediaError::Cancelled);
        }
        self.present_only(video_frame)
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
        #[cfg(feature = "thumbs")]
        if let Ok(slot) = self.shared.thumb.try_borrow()
            && let Some(thumbs) = slot.as_ref()
        {
            thumbs.clear();
        }
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
        if self.shared.engine.get().is_play()
            || self.is_playing()
            || !self.scrub_policy().densify
            || self.shared.scrub_densify_blocked.get()
        {
            return;
        }
        if self.shared.engine.get().holds_exact() || self.shared.scrub_busy.get() {
            self.shared.scrub_pending.set(true);
            return;
        }
        self.kick_scrub_cache_fill();
    }

    pub(super) fn kick_scrub_cache_fill(&self) {
        if self.shared.engine.get().is_play()
            || self.is_playing()
            || !self.scrub_policy().densify
            || self.shared.scrub_densify_blocked.get()
            || self.shared.engine.get().holds_exact()
        {
            if self.shared.engine.get().holds_exact() {
                self.shared.scrub_pending.set(true);
            }
            return;
        }
        if self.shared.scrub_busy.get() {
            self.shared.scrub_pending.set(true);
            return;
        }
        self.shared.scrub_busy.set(true);
        self.shared.scrub_pending.set(false);
        let shared = Rc::clone(&self.shared);
        wasm_bindgen_futures::spawn_local(async move {
            let _ = scrub_cache_fill_task(shared).await;
        });
    }
}

/// Next missing sync key for sparse thumb fill (focus = hover aim).
#[cfg(feature = "thumbs")]
pub(super) fn next_thumb_key_gap_shared(shared: &Shared) -> Option<FrameId> {
    let thumbs = shared.thumb.try_borrow().ok().and_then(|s| s.clone())?;
    let meta = shared.player.borrow().metadata().clone();
    let focus = thumbs.hover_aim();
    let keys = shared.player.borrow().track().index().sync_frame_ids();
    thumbs.next_missing_key(&keys, focus, meta.width, meta.height)
}

pub(super) async fn scrub_cache_fill_task(shared: Rc<Shared>) -> Result<(), MediaError> {
    let mut scrub_epoch = shared.scrub_epoch.get();
    let result = async {
        let mut idle_passes = 0u8;
        loop {
            if shared.engine.get().is_play()
                || shared.player.borrow().state().playing
                || !shared.player.borrow().metadata().scrub_policy().densify
                || shared.scrub_densify_blocked.get()
            {
                return Ok(());
            }
            // Settle / sync seek owns HW — exit so `scrub_busy` clears and the scrub
            // decoder is not pinned in a spin-wait (was starving exact on long GOPs).
            if shared.engine.get().holds_exact() {
                shared.scrub_pending.set(true);
                return Ok(());
            }
            // Pause/play bumped the fill epoch — adopt it and keep going if still scrubbing.
            if shared.scrub_epoch.get() != scrub_epoch {
                if !shared.engine.get().allows_densify() || shared.player.borrow().state().playing {
                    return Ok(());
                }
                scrub_epoch = shared.scrub_epoch.get();
                // Do **not** clear `scrub_pending` — abort(rearm)/schedule may have
                // latched another pass while we were cancelled mid-GOP.
            }

            apply_clip_budgets(&shared);
            let Some(target) = next_gap_target(&shared) else {
                // Pending kick may have been set while we were at a momentary full state.
                if shared.scrub_pending.get() {
                    shared.scrub_pending.set(false);
                    idle_passes = 0;
                    sleep_ms(0).await;
                    continue;
                }
                return Ok(());
            };

            // Cold open / URL: do not densify-decode until the GOP's sample bytes
            // are resident (FetchMap) and the frame is indexed. Otherwise soft-drag
            // right after open stamps Ranges + decode + ImageBitmap and freezes the
            // tab. Prefetch / mfra backfill run; densify retries next pass.
            // Local invention (mediabunny has no densify plane).
            let key = shared
                .player
                .borrow()
                .track()
                .index()
                .nearest_keyframe(target)
                .unwrap_or(target);
            if !scrub_gop_bytes_warm(&shared, key) {
                warm_scrub_gop_bytes(&shared, key);
                idle_passes = 0;
                wait_animation_frame().await;
                continue;
            }
            // Soft cold settle: GOP bytes arrived — paint Exact blit if densify
            // already has the aim (or after the fill below).
            try_resolve_pending_cold_exact(&shared);

            match fill_one_gap(Rc::clone(&shared), target, scrub_epoch).await {
                Ok(made_progress) => {
                    idle_passes = if made_progress {
                        0
                    } else {
                        idle_passes.saturating_add(1)
                    };
                    // No progress repeatedly (oversized GOP / budget) — stop spinning.
                    if idle_passes >= 3 {
                        shared
                            .fill_stalled_aim
                            .set(Some(playhead_and_last(&shared).0));
                        return Ok(());
                    }
                    if made_progress {
                        try_resolve_pending_cold_exact(&shared);
                        // One display frame after a GOP of VF→ImageBitmap so the
                        // GPU process can reclaim before the next burst (Chrome
                        // whole-window black flash under densify VRAM pressure).
                        // Keep Exact blit at viewport size — do not shrink the window.
                        // mediabunny: CanvasSink poolSize stays tiny; no equivalent
                        // pacing — local yield only (`sleep_ms(0)` was too short).
                        wait_animation_frame().await;
                    } else {
                        // Macrotask so paused rAF can refresh the ready strip.
                        sleep_ms(0).await;
                    }
                }
                Err(MediaError::Cancelled) => {
                    // Settle/seek hold — exit so scrub HW yields immediately
                    // (do not spin/sleep; was competing with exact after long drag).
                    if shared.engine.get().holds_exact() {
                        shared.scrub_pending.set(true);
                        return Ok(());
                    }
                    // scrub_epoch bump (abort) — adopt and keep densifying while scrubbing.
                    idle_passes = 0;
                    sleep_ms(0).await;
                }
                Err(_) => {
                    // Transient WebCodecs fault — keep densifying (do not kill the task).
                    idle_passes = idle_passes.saturating_add(1);
                    if idle_passes >= 3 {
                        shared
                            .fill_stalled_aim
                            .set(Some(playhead_and_last(&shared).0));
                        return Ok(());
                    }
                    sleep_ms(0).await;
                }
            }
        }
    }
    .await;
    shared.scrub_busy.set(false);

    // While exact settle/seek holds densify, leave `scrub_pending` latched and do
    // **not** respawn — a spawn_local spin froze the tab (long-GOP cold scrub).
    if shared.engine.get().holds_exact() {
        return result;
    }

    let want_again = shared.scrub_pending.get()
        && shared.engine.get().allows_densify()
        && !shared.player.borrow().state().playing
        && next_gap_target(&shared).is_some();
    shared.scrub_pending.set(false);

    if want_again && !shared.scrub_busy.get() {
        shared.scrub_busy.set(true);
        let again = Rc::clone(&shared);
        wasm_bindgen_futures::spawn_local(async move {
            yield_event_loop().await;
            let _ = scrub_cache_fill_task(again).await;
        });
    }
    result
}

pub(super) fn apply_clip_budgets(shared: &Shared) {
    let meta = shared.player.borrow().metadata().clone();
    let policy = meta.scrub_policy();
    let max_frames = meta.scrub_cache_frame_budget();
    shared.scrub_chunks.borrow_mut().set_max_frames(max_frames);
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

pub(super) fn playhead_and_last(shared: &Shared) -> (FrameId, FrameId) {
    let state = shared.player.borrow().state();
    let last = state.frame_count.saturating_sub(1);
    let committed = state.frame.min(last);
    let aimed = shared.scrub_chunks.borrow().playhead().min(last);
    // During scrub, aim follows the pointer; otherwise the committed playhead.
    let playhead = if aimed != committed { aimed } else { committed };
    (playhead, last)
}

/// Next missing frame nearest the playhead (biased by scrub `dir`), or `None`
/// when saturated / disarmed / stalled at this aim.
pub(super) fn next_gap_target(shared: &Shared) -> Option<FrameId> {
    let (playhead, last) = playhead_and_last(shared);
    // fMP4 warm open: provisional duration may exceed indexed samples — never
    // densify past the indexed prefix (would stall on mfra backfill + decode).
    let indexed_last = shared
        .player
        .borrow()
        .track()
        .indexed_frame_count()
        .saturating_sub(1);
    let last = last.min(indexed_last);
    let playhead = playhead.min(last);
    if shared.fill_stalled_aim.get() == Some(playhead) {
        return None;
    }
    if shared.fill_stalled_aim.get().is_some() {
        // Aim moved off the stalled frame — fill may run again.
        shared.fill_stalled_aim.set(None);
    }
    let dir = shared.scrub_dir.get();
    let policy = shared.player.borrow().metadata().scrub_policy();
    let (margin_left, margin_right) = (policy.margin_left, policy.margin_right);
    // Play keeps markers and trims Exact (not a full wipe) — ImageBitmap holes
    // must still win over marker `no_gaps`, or pause densify never re-warms Exact.
    if let Some(blit_gap) =
        shared
            .scrub_blit
            .borrow()
            .next_gap_frame(playhead, last, dir, margin_left, margin_right)
    {
        return Some(blit_gap);
    }
    let cache = shared.scrub_chunks.borrow();
    if cache.gaps_saturated(playhead, last, dir)
        || !cache.gap_fill_armed(playhead, last, margin_left, margin_right)
    {
        return None;
    }
    cache.next_gap_frame(playhead, last, dir)
}

/// True when every sample in the GOP at `key` is indexed and resident in
/// [`FetchMap`] (CachedSource windows). Cold → densify must prefetch, not decode.
fn scrub_gop_bytes_warm(shared: &Shared, key: FrameId) -> bool {
    let track = shared.player.borrow().track_rc();
    let indexed = track.indexed_frame_count();
    if key >= indexed {
        return false;
    }
    let index = track.index();
    let ids = index.gop_frame_ids(key).unwrap_or_default();
    if ids.is_empty() {
        return false;
    }
    if ids.iter().any(|&id| id >= indexed) {
        return false;
    }
    for id in ids {
        let Some(entry) = index.get(id) else {
            return false;
        };
        let end = entry.offset.saturating_add(entry.size);
        if !shared.fetch_map.covers_range(entry.offset, end) {
            return false;
        }
    }
    true
}

/// Settle/seek may run Exact cascade only when aim GOP samples are indexed + resident.
///
/// Cold → soft present + prefetch (`soft_cold_scrub_settle`); densify paints later.
/// Avoids `goto_frame_with_interim` → `ensure_frame_indexed` + key→N on cold Ranges
/// (tab freeze on rapid taps). Local invention (no densify plane in mediabunny);
/// async retrieve mirrors `mediabunny: src/media-sink.ts` `mediaSamplesAtTimestamps`
/// (await packets without blocking the host on every seek).
pub(super) fn exact_scrub_ready(shared: &Shared, frame: FrameId) -> bool {
    let track = shared.player.borrow().track_rc();
    let indexed = track.indexed_frame_count();
    if frame >= indexed {
        return false;
    }
    let key = track.index().nearest_keyframe(frame).unwrap_or(frame);
    scrub_gop_bytes_warm(shared, key)
}

/// Soft settle/seek on a cold frame: prefetch only, optional warm blit, commit
/// only within the **indexed** prefix.
///
/// Does **not** decode. Never `commit_frame` past `indexed_frame_count` — that
/// left PlayPrime / `warm_sample_bytes` awaiting mfra on the OpQueue (Play stuck
/// `busy`). User aim stays in `pending_cold_exact`; densify paints when warm.
/// Canvas may stay on the prior frame until Exact blit lands.
pub(super) fn soft_cold_scrub_settle(
    player: &VideoPlayer,
    frame: FrameId,
) -> Result<(), MediaError> {
    let indexed = player.shared.player.borrow().track().indexed_frame_count();
    let key = player.key_for(frame).unwrap_or(frame);
    warm_scrub_gop_bytes(&player.shared, key);
    // Soft present: nearest warm blit only — never cold decode on the OpQueue.
    if let Some(preview) = player.scrub_preview_frame(frame) {
        let _ = player.try_present_warm_blit(preview)?;
    }
    if indexed > 0 {
        let commit = frame.min(indexed - 1);
        player.shared.player.borrow_mut().commit_frame(commit)?;
        player.touch_playhead(commit);
    }
    Ok(())
}

/// When soft cold settle deferred Exact and densify now has the aim blit, paint it.
fn try_resolve_pending_cold_exact(shared: &Rc<Shared>) {
    let Some(aim) = shared.pending_cold_exact.get() else {
        return;
    };
    if !exact_scrub_ready(shared, aim) {
        return;
    }
    let Ok(mut blit) = shared.scrub_blit.try_borrow_mut() else {
        return;
    };
    let Some(bitmap) = blit.get(aim) else {
        return;
    };
    if draw_image_bitmap(&shared.canvas, &shared.ctx, bitmap).is_ok() {
        let _ = shared.player.borrow_mut().commit_frame(aim);
        shared.pending_cold_exact.set(None);
    }
}

/// Kick sample prefetch (and fragment backfill if needed) for a cold densify GOP.
fn warm_scrub_gop_bytes(shared: &Shared, key: FrameId) {
    let track = shared.player.borrow().track_rc();
    let indexed = track.indexed_frame_count();
    if key >= indexed {
        track.kick_fragment_backfill();
        return;
    }
    let end = track
        .index()
        .gop_frame_ids(key)
        .unwrap_or_default()
        .last()
        .copied()
        .unwrap_or(key)
        .saturating_add(1)
        .min(indexed);
    track.kick_prefetch_samples(key as usize, end as usize);
}

/// Densify the GOP containing `target`. Returns whether the cache gained frames.
///
/// Once a GOP decode starts, **finish it** even if the scrub aim moves — rapid
/// drag used to cancel mid-GOP (≥8 frames / other key) and left dense empty.
/// Aim only picks the *next* gap after this GOP completes. Still abort on settle
/// hold, play, or scrub-epoch bump.
pub(super) async fn fill_one_gap(
    shared: Rc<Shared>,
    target: FrameId,
    scrub_epoch: u64,
) -> Result<bool, MediaError> {
    let key = shared
        .player
        .borrow()
        .track()
        .index()
        .nearest_keyframe(target)
        .unwrap_or(target);

    let cancelled = || {
        shared.scrub_epoch.get() != scrub_epoch
            || shared.engine.get().is_play()
            || shared.player.borrow().state().playing
            || shared.engine.get().holds_exact()
    };
    if cancelled() {
        return Err(MediaError::Cancelled);
    }
    {
        let index = shared.player.borrow().track().index().clone();
        let ids = index.gop_frame_ids(key).unwrap_or_default();
        // Skip only when markers **and** live blit cover the GOP. Markers alone
        // after Play's blit wipe must not count as done.
        let cache = shared.scrub_chunks.borrow();
        let blit = shared.scrub_blit.borrow();
        let blit_complete = !ids.is_empty() && ids.iter().all(|id| blit.contains(*id));
        if !ids.is_empty() && cache.gop_complete(&ids) && cache.has_frame(target) && blit_complete {
            return Ok(true);
        }
    }

    let meta = shared.player.borrow().metadata().clone();
    let track = shared.player.borrow().track_rc();

    // Short borrow for take — never hold scrub_decoder RefMut across await/reset.
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
        *shared.scrub_decoder.borrow_mut() = Some(decoder);
        return Err(MediaError::Cancelled);
    }

    // Stream densify markers + blit as each remapped frame emerges so the ready
    // strip paints per-frame (not one tick-burst after the whole GOP).
    // Progress ≠ frame_count growth: at capacity, evict+insert keeps len stable.
    let shared_for_cache = Rc::clone(&shared);
    let newly = std::rc::Rc::new(std::cell::Cell::new(0u32));
    let newly_cb = Rc::clone(&newly);
    let mut on_frame = move |id: FrameId, video: OwnedVideoFrame| {
        if ingest_scrub_frame_shared(&shared_for_cache, id, video, false) {
            newly_cb.set(newly_cb.get().saturating_add(1));
        }
    };
    let decoded = decoder
        .decode_gop(&track, key, &cancelled, &mut on_frame)
        .await;
    match decoded {
        Ok(()) => {
            *shared.scrub_decoder.borrow_mut() = Some(decoder);
            let got_target = shared
                .scrub_chunks
                .try_borrow()
                .map(|c| c.has_frame(target))
                .unwrap_or(false);
            Ok(newly.get() > 0 || got_target)
        }
        Err(MediaError::Cancelled) => {
            let _ = decoder.reset();
            *shared.scrub_decoder.borrow_mut() = Some(decoder);
            Err(MediaError::Cancelled)
        }
        Err(e) => {
            let _ = decoder.reset();
            *shared.scrub_decoder.borrow_mut() = Some(decoder);
            Err(e)
        }
    }
}
