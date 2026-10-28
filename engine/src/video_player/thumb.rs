//! Hover / filmstrip thumbs for [`super::VideoPlayer`] (feature `thumbs`).
//! Stubs for the feature-off build live in `thumb_off.rs`.

use std::rc::Rc;

use web_sys::HtmlCanvasElement;

use crate::decode::thumb_sink::{
    ThumbSink, ThumbSinkOptions, paint_thumb_canvas, run_sparse_key_fill,
};
use crate::error::MediaError;
use crate::policy::workload::{
    PLAY_THUMB_AIM_DECODE_MIN_MS, allows_thumb_aim_decode, allows_thumb_key_fill,
};
use crate::policy::{ThumbPolicy, hover_thumb_dims, scrub_preview_choice_with_dir};
use crate::types::FrameId;

use super::{Shared, VideoPlayer, now_ms, sleep_ms};

impl VideoPlayer {
    /// Abort in-flight hover Exact downscale (scrub start / pointer leave).
    /// Play does not cancel the sink — ThumbCache stays; sparse fill yields via
    /// `allows_thumb_key_fill`, aim decode is throttled. `mediabunny:` separate thumb `CanvasSink`.
    pub fn cancel_hover_thumb(&self) {
        if let Ok(slot) = self.shared.thumb.try_borrow()
            && let Some(preview) = slot.as_ref()
        {
            preview.cancel();
        }
    }

    /// Runtime thumb gate. `Off` drops the lazy [`ThumbSink`].
    pub fn set_thumb_policy(&self, policy: ThumbPolicy) {
        self.shared.thumb_policy.set(policy);
        if policy == ThumbPolicy::Off {
            self.cancel_hover_thumb();
            if let Ok(mut slot) = self.shared.thumb.try_borrow_mut() {
                let _ = slot.take();
            }
        }
    }

    /// Leased [`ThumbSink`] when policy is On (same instance hover/fill use).
    ///
    /// Hosts may also build a sink with [`ThumbSink::new`] without a player.
    pub fn thumb_sink(&self) -> Option<Rc<ThumbSink>> {
        self.thumb_sink_inner()
    }

    pub fn thumb_policy(&self) -> ThumbPolicy {
        self.shared.thumb_policy.get()
    }

    /// Drop cached thumbs (open / close teardown — **not** loop).
    pub(super) fn clear_thumbs(&self) {
        if let Ok(slot) = self.shared.thumb.try_borrow()
            && let Some(thumbs) = slot.as_ref()
        {
            thumbs.clear();
        }
    }

    /// Dispose: cancel epoch + clear before releasing the sink `Rc` so in-flight
    /// aim/fill see the generation bump (not only `Shared.disposed`).
    pub(super) fn drop_thumb_sink(&self) {
        if let Ok(mut slot) = self.shared.thumb.try_borrow_mut() {
            if let Some(thumbs) = slot.as_ref() {
                thumbs.clear();
            }
            *slot = None;
        }
    }

    /// Ensure a [`ThumbSink`] exists when policy is `On`. Returns `false` if off / unavailable.
    pub(super) fn ensure_thumb_sink(&self) -> bool {
        if self.shared.thumb_policy.get() != ThumbPolicy::On {
            return false;
        }
        let Ok(mut slot) = self.shared.thumb.try_borrow_mut() else {
            return false;
        };
        if slot.is_none() {
            *slot = Some(Rc::new(ThumbSink::new(
                Rc::clone(&self.shared.track),
                ThumbSinkOptions::default(),
            )));
        }
        true
    }

    /// Sync hover paint from [`ThumbSink`] / nearest available preview.
    ///
    /// Picks ThumbCache vs Exact via [`scrub_preview_choice_with_dir`] (same
    /// floor/ceil-by-motion rule as soft-drag). When Exact wins, downscales into
    /// ThumbCache **synchronously** (cheap `bitmap_to_sized`) so pointermove
    /// never leaves a farther thumb on the canvas. `mediabunny:` present cached
    /// canvas immediately; decode only on true miss. Allowed while playing.
    pub fn try_paint_hover_thumb_cached(
        &self,
        frame: FrameId,
        dest: &HtmlCanvasElement,
        max_w: u32,
        max_h: u32,
    ) -> Result<Option<FrameId>, MediaError> {
        if !self.ensure_thumb_sink() {
            return Err(MediaError::Cancelled);
        }
        let count = self.state().frame_count;
        if count == 0 {
            return Err(MediaError::FrameOutOfRange(frame, count));
        }
        let frame = frame.min(count.saturating_sub(1));
        let dir = {
            let Ok(slot) = self.shared.thumb.try_borrow() else {
                return Ok(None);
            };
            let Some(thumbs) = slot.as_ref() else {
                return Ok(None);
            };
            thumbs.note_size(max_w, max_h);
            thumbs.set_hover_aim(frame);
            thumbs.hover_dir()
        };
        let meta = self.shared.track.metadata();
        let (tw, th) = hover_thumb_dims(meta.width, meta.height, max_w, max_h);

        let cached_exact = self
            .shared
            .scrub_blit
            .try_borrow()
            .ok()
            .and_then(|b| b.nearest_frame_with_dir(frame, dir));
        let cached_thumb = {
            let Ok(slot) = self.shared.thumb.try_borrow() else {
                return Ok(None);
            };
            let Some(thumbs) = slot.as_ref() else {
                return Ok(None);
            };
            thumbs.nearest_frame_with_dir(frame, tw, th, dir)
        };
        let Some(preview) = scrub_preview_choice_with_dir(frame, cached_thumb, cached_exact, dir)
        else {
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
                        match thumbs.with_nearest_bitmap_dir(frame, tw, th, dir, |bmp| {
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
                match thumbs.with_nearest_bitmap_dir(frame, tw, th, dir, |bmp| {
                    paint_thumb_canvas(dest, bmp, tw, th)
                }) {
                    Some((id, Ok(()))) => Ok(Some(id)),
                    Some((_, Err(e))) => Err(e),
                    None => Ok(None),
                }
            }
        };
        if matches!(&result, Ok(Some(_))) && allows_thumb_key_fill(self.shared.workload_hints()) {
            self.schedule_thumb_key_fill();
        }
        result
    }

    /// Paint a hover preview into `dest`.
    ///
    /// Sync nearest (ThumbCache ∪ Exact) first. Sparse key fill only when paused
    /// ([`allows_thumb_key_fill`]). Cold miss → aim decode (free on Pause, Play
    /// floor). Far interim → aim decode with [`PLAY_THUMB_AIM_DECODE_MIN_MS`] on
    /// **both** Pause and Play (Exact-at-0 after open must not storm the 3rd
    /// decoder against densify). Seek blocks aim decode. `mediabunny:` present
    /// cached canvas immediately; sparse fill off present path.
    pub async fn paint_hover_thumb(
        &self,
        frame: FrameId,
        dest: &HtmlCanvasElement,
        max_w: u32,
        max_h: u32,
    ) -> Result<FrameId, MediaError> {
        if !self.ensure_thumb_sink() {
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
        if allows_thumb_key_fill(self.shared.workload_hints()) {
            self.schedule_thumb_key_fill();
        }
        if let Some(p) = interim {
            if self.hover_thumb_close_enough(frame, p) {
                return Ok(p);
            }
            // Far interim on canvas — always throttle refine (Pause+Play).
            if !self.may_start_hover_aim_decode(true) {
                return Ok(p);
            }
            return self
                .decode_hover_thumb(frame, dest, max_w, max_h, true)
                .await;
        }

        // Cold miss — Pause free; Play floor.
        if !self.may_start_hover_aim_decode(false) {
            return Err(MediaError::Cancelled);
        }
        self.decode_hover_thumb(frame, dest, max_w, max_h, false)
            .await
    }

    /// Whether a hover-aim decode may start now.
    ///
    /// Seek blocks. Cold miss (`far_refine=false`): free on Pause, floor on Play.
    /// Far refine: floor on Pause **and** Play — after open Exact frame 0 is a
    /// far hit for almost every aim; unthrottled Pause refine froze the tab.
    fn may_start_hover_aim_decode(&self, far_refine: bool) -> bool {
        let hints = self.shared.workload_hints();
        if !allows_thumb_aim_decode(hints) {
            return false;
        }
        if !far_refine && allows_thumb_key_fill(hints) {
            return true;
        }
        let now = now_ms().unwrap_or(0.0);
        let Ok(slot) = self.shared.thumb.try_borrow() else {
            return false;
        };
        let Some(thumbs) = slot.as_ref() else {
            return false;
        };
        now - thumbs.last_aim_decode_ms() >= PLAY_THUMB_AIM_DECODE_MIN_MS
    }

    /// Exact blit → sized thumb into [`ThumbSink`] (read-only densify plane).
    ///
    /// Only runs when Exact is the [`scrub_preview_choice_with_dir`] winner vs
    /// ThumbCache (caller already tried cache). Localizes to directional Exact
    /// neighbor of aim.
    pub(super) fn try_hover_thumb_from_exact(
        &self,
        frame: FrameId,
        dest: &HtmlCanvasElement,
        max_w: u32,
        max_h: u32,
    ) -> Result<Option<FrameId>, MediaError> {
        let dir = self
            .shared
            .thumb
            .try_borrow()
            .ok()
            .and_then(|s| s.as_ref().map(|t| t.hover_dir()))
            .unwrap_or(0);
        let cached_exact = self
            .shared
            .scrub_blit
            .try_borrow()
            .ok()
            .and_then(|b| b.nearest_frame_with_dir(frame, dir));
        let Some(preview) = scrub_preview_choice_with_dir(frame, None, cached_exact, dir) else {
            return Ok(None);
        };
        let store_key = self.key_for(preview).unwrap_or(preview);
        let meta = self.shared.track.metadata();
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
    ///
    /// `far_refine` selects the aim-decode throttle (see [`Self::may_start_hover_aim_decode`]).
    pub(super) async fn decode_hover_thumb(
        &self,
        frame: FrameId,
        dest: &HtmlCanvasElement,
        max_w: u32,
        max_h: u32,
        far_refine: bool,
    ) -> Result<FrameId, MediaError> {
        if !self.may_start_hover_aim_decode(far_refine) {
            return Err(MediaError::Cancelled);
        }
        // Stamp before claim so concurrent paints see the Play floor.
        if let Ok(slot) = self.shared.thumb.try_borrow()
            && let Some(thumbs) = slot.as_ref()
        {
            thumbs.note_aim_decode_ms(now_ms().unwrap_or(0.0));
        }
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
            // Bg key-fill owns the worker — wait until free; latest aim already
            // latched in `pending_aim`. Do **not** spin on `yield_microtask`
            // (Promise.resolve microtask) — that freezes the tab while fill
            // decodes. Macrotask + wall cap; pending survives for the next paint.
            let wait_epoch = {
                let Ok(slot) = self.shared.thumb.try_borrow() else {
                    return Err(MediaError::Cancelled);
                };
                slot.as_ref()
                    .map(|t| t.generation())
                    .ok_or(MediaError::Cancelled)?
            };
            let wait_start = now_ms().unwrap_or(0.0);
            const CLAIM_WAIT_MS: f64 = 2_000.0;
            loop {
                sleep_ms(0).await;
                if now_ms().unwrap_or(wait_start) - wait_start >= CLAIM_WAIT_MS {
                    return Err(MediaError::Cancelled);
                }
                if !allows_thumb_aim_decode(self.shared.workload_hints()) {
                    return Err(MediaError::Cancelled);
                }
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

        let meta = self.shared.track.metadata();
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

        let Some(thumbs) = self.thumb_sink_inner() else {
            return Err(MediaError::Cancelled);
        };
        thumbs.note_size(max_w, max_h);
        thumbs.decode_into_cache(frame, cancelled).await?;
        if cancelled() {
            return Err(MediaError::Cancelled);
        }
        let paint_aim = thumbs.hover_aim();
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
    ///
    /// Pause/idle only ([`allows_thumb_key_fill`]). Play uses throttled single-aim
    /// decode instead — no fill chain.
    pub(super) fn schedule_thumb_key_fill(&self) {
        if self.shared.is_disposed() || self.shared.scrub_bg_suspended() {
            return;
        }
        if !allows_thumb_key_fill(self.shared.workload_hints()) {
            return;
        }
        if !self.ensure_thumb_sink() {
            return;
        }
        let Some(thumbs) = self.thumb_sink_inner() else {
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

    pub(super) fn thumb_sink_inner(&self) -> Option<Rc<ThumbSink>> {
        self.shared.thumb.try_borrow().ok().and_then(|s| s.clone())
    }

    pub(super) fn kick_thumb_key_fill(&self) {
        if self.shared.is_disposed() || self.shared.scrub_bg_suspended() {
            return;
        }
        if !allows_thumb_key_fill(self.shared.workload_hints()) {
            return;
        }
        let Some(key) = self.next_thumb_key_gap() else {
            return;
        };
        let Some(thumbs) = self.thumb_sink_inner() else {
            return;
        };
        if !thumbs.claim_fill_key(key) {
            return;
        }
        let shared = Rc::clone(&self.shared);
        let sink = Rc::clone(&thumbs);
        wasm_bindgen_futures::spawn_local(async move {
            if shared.is_disposed() {
                return;
            }
            let yield_play = || {
                shared.is_disposed()
                    || shared.scrub_bg_suspended()
                    || !allows_thumb_key_fill(shared.workload_hints())
            };
            let next_gap = || {
                if yield_play() {
                    return None;
                }
                next_thumb_key_gap_shared(&shared)
            };
            let _ = run_sparse_key_fill(sink, next_gap, yield_play).await;
        });
    }

    /// Next sync frame missing from ThumbCache, nearest to hover aim / playhead.
    pub(super) fn next_thumb_key_gap(&self) -> Option<FrameId> {
        next_thumb_key_gap_shared(&self.shared)
    }
}

/// Next missing sync key for sparse thumb fill (focus = hover aim).
fn next_thumb_key_gap_shared(shared: &Shared) -> Option<FrameId> {
    let thumbs = shared.thumb.try_borrow().ok().and_then(|s| s.clone())?;
    let meta = shared.track.metadata();
    let focus = thumbs.hover_aim();
    let keys = shared.track.index().sync_frame_ids();
    thumbs.next_missing_key(&keys, focus, meta.width, meta.height)
}
