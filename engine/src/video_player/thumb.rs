//! Hover / filmstrip thumbs for [`super::VideoPlayer`] (feature `thumbs`).
//! Stubs for the feature-off build live in `thumb_off.rs`.

use std::rc::Rc;

use web_sys::HtmlCanvasElement;

use crate::decode::thumb_sink::{
    ThumbSink, ThumbSinkOptions, paint_thumb_canvas, run_sparse_key_fill,
};
use crate::error::MediaError;
use crate::policy::workload::{allows_thumb_aim_decode, allows_thumb_key_fill};
use crate::policy::{ThumbPolicy, hover_thumb_dims, scrub_preview_choice_with_dir};
use crate::types::FrameId;

use super::{Shared, VideoPlayer, now_ms, sleep_ms};

/// Wall cap waiting for a free PreferSoftware claim (no microtask spin).
const CLAIM_WAIT_MS: f64 = 2_000.0;

impl VideoPlayer {
    /// Abort in-flight hover Exact downscale (scrub start / pointer leave).
    /// Play does not cancel the sink — ThumbCache stays; fill/aim decode only
    /// when paused. `resume_progressive` restarts Pause fill after hover leave.
    /// `mediabunny:` separate thumb `CanvasSink`.
    pub fn cancel_hover_thumb(&self) {
        self.cancel_hover_thumb_inner(true);
    }

    fn cancel_hover_thumb_inner(&self, resume_progressive: bool) {
        if let Ok(slot) = self.shared.thumb.try_borrow()
            && let Some(preview) = slot.as_ref()
        {
            preview.cancel();
        }
        if resume_progressive {
            self.schedule_thumb_key_fill();
        }
    }

    /// Runtime thumb gate. `Off` drops the lazy [`ThumbSink`].
    pub fn set_thumb_policy(&self, policy: ThumbPolicy) {
        self.shared.thumb_policy.set(policy);
        if policy == ThumbPolicy::Off {
            self.cancel_hover_thumb_inner(false);
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
    /// canvas immediately; decode only on true miss (Pause). Play: cache/Exact only.
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
        match painted {
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
        }
    }

    /// Paint a hover preview into `dest`.
    ///
    /// Sync nearest (ThumbCache ∪ Exact) first. Aim decode only when paused
    /// ([`allows_thumb_aim_decode`]). Progressive Pause fill suspends while
    /// hover is active. Play: cache / Exact bridge only. Key/far interim on
    /// Pause → Exact aim decode after host ~150 ms coalesce. Seek blocks.
    /// `mediabunny:` present cached canvas immediately.
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
        if let Some(p) = interim {
            // Exact frame already — done. Same-GOP key still far-refines on Pause.
            if self.hover_thumb_close_enough(frame, p) {
                return Ok(p);
            }
            // Key / far interim — throttle Exact refine (Pause; Play refuses).
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
    /// Play / Seek blocked ([`allows_thumb_aim_decode`]). Pause cold miss **and**
    /// Exact-on-dwell refine are free — host `<HoverThumb>` already coalesces
    /// ~150 ms. A global 800 ms floor was wrong: one trailing paint bails on the
    /// floor, keeps the key interim, and nothing reschedules while the pointer
    /// holds still. Progressive suspends on hover so densify thrash is limited.
    fn may_start_hover_aim_decode(&self, _far_refine: bool) -> bool {
        allows_thumb_aim_decode(self.shared.workload_hints())
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
        last_ok.ok_or(MediaError::Cancelled)
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

    /// Kick Pause progressive key → ThumbCache fill (same 3rd decoder; hover wins).
    ///
    /// Pause/idle only ([`allows_thumb_key_fill`]). Suspended while hover active;
    /// stops forever once progressive hits cap / no candidates. Play: no fill.
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
        if thumbs.decode_blocked() || thumbs.progressive_done() || thumbs.hover_active() {
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
        let Some(thumbs) = self.thumb_sink_inner() else {
            return;
        };
        if thumbs.hover_active() || thumbs.progressive_done() {
            return;
        }
        let Some(key) = self.next_thumb_progressive_key() else {
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
                    || shared
                        .thumb
                        .try_borrow()
                        .ok()
                        .and_then(|s| s.as_ref().map(|t| t.hover_active()))
                        .unwrap_or(false)
            };
            let next_gap = || {
                if yield_play() {
                    return None;
                }
                next_thumb_progressive_key_shared(&shared)
            };
            let _ = run_sparse_key_fill(sink, next_gap, yield_play).await;
        });
    }

    /// Next progressive sync key for Pause fill.
    pub(super) fn next_thumb_progressive_key(&self) -> Option<FrameId> {
        next_thumb_progressive_key_shared(&self.shared)
    }
}

/// Next Pause progressive sync key (coarse → farthest densify).
fn next_thumb_progressive_key_shared(shared: &Shared) -> Option<FrameId> {
    let thumbs = shared.thumb.try_borrow().ok().and_then(|s| s.clone())?;
    if thumbs.hover_active() || thumbs.progressive_done() {
        return None;
    }
    let meta = shared.track.metadata();
    let keys = shared.track.index().sync_frame_ids();
    thumbs.next_progressive_key(&keys, meta.width, meta.height)
}
