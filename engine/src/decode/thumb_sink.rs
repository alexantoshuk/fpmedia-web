//! Standalone thumb / filmstrip sink — **product surface**, not play/scrub.
//!
//! `mediabunny:` `CanvasSink` (`getCanvas` / `canvasesAtTimestamps`) — separate
//! PreferSoftware decoder + sized bitmaps. Rust deltas: [`FrameId`] primary,
//! [`Result`] + RAII `ImageBitmap::close`, residency [`ThumbCache`] (not ring
//! `poolSize`), sync peek for hover paint.
//!
//! Usable without [`crate::VideoPlayer`]: `open_mp4` → track → [`ThumbSink::new`].
//! Player hover / sparse fill are **clients** (Exact bridge + workload gates stay
//! on `VideoPlayer`).

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use web_sys::{HtmlCanvasElement, ImageBitmap};

use super::canvas::canvas_2d;
use super::frame_cache::{DEFAULT_MAX_THUMB_GOPS, DEFAULT_MAX_THUMBS, ThumbCache};
use super::owned_video_frame::AsVideoFrame;
use super::scrub_blit::{BlitCanvas, BlitCanvasSlot};
use super::webcodecs::WebCodecsDecoder;
use crate::demux::Mp4VideoTrack;
use crate::error::MediaError;
use crate::model::bg_task::SingleFlight;
use crate::policy::{
    hover_thumb_dims, next_missing_sync_key, next_progressive_sync_key,
    thumb_progressive_coarse_count, thumb_progressive_rank,
};
use crate::types::FrameId;

/// Default hover box (matches `fpmedia_web_components` / `.fpvideo-thumb-preview`).
pub const DEFAULT_THUMB_MAX_W: u32 = 160;
pub const DEFAULT_THUMB_MAX_H: u32 = 90;

/// Options for [`ThumbSink`] (`mediabunny:` `CanvasSinkOptions` width/height).
///
/// `max_gops` / `max_thumbs` are **local** residency (MB has canvas `poolSize` for
/// iterator reuse — we keep sized bitmaps for hover/filmstrip).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ThumbSinkOptions {
    pub max_w: u32,
    pub max_h: u32,
    pub max_gops: usize,
    pub max_thumbs: usize,
}

impl Default for ThumbSinkOptions {
    fn default() -> Self {
        Self {
            max_w: DEFAULT_THUMB_MAX_W,
            max_h: DEFAULT_THUMB_MAX_H,
            max_gops: DEFAULT_MAX_THUMB_GOPS,
            max_thumbs: DEFAULT_MAX_THUMBS,
        }
    }
}

/// Thumb generation sink (`mediabunny:` [`CanvasSink`](https://mediabunny.dev/api/CanvasSink)).
///
/// Product API: [`Self::get`] / [`Self::at_frames`] / sync peek. Not the play or
/// densify decoder.
pub struct ThumbSink {
    track: Rc<Mp4VideoTrack>,
    cache: RefCell<ThumbCache>,
    /// Bumped to cancel in-flight decode / Exact downscale clients.
    epoch: Cell<u64>,
    decoder: RefCell<Option<WebCodecsDecoder>>,
    /// Own blit surface — never share densify `encode_canvas` (RefCell re-entry).
    blit: BlitCanvasSlot,
    /// HEVC HW-only: refuse decode for this clip (Exact downscale only).
    decode_blocked: Cell<bool>,
    /// Exclusive decode worker (latest-wins via [`Self::pending_aim`] for clients).
    flight: SingleFlight,
    /// Newest aim while a worker is running (presentation / key frame).
    pending_aim: Cell<Option<FrameId>>,
    /// Hover client: paint nearest-to-this after decode.
    hover_aim: Cell<FrameId>,
    /// Hover client: floor/ceil bias (`+1` / `-1` / `0`).
    hover_dir: Cell<i8>,
    prev_hover_aim: Cell<Option<FrameId>>,
    last_max_w: Cell<u32>,
    last_max_h: Cell<u32>,
    /// Hover client: wall ms of last aim-decode start (Play throttle).
    last_aim_decode_ms: Cell<f64>,
    /// Pointer over scrubber — progressive Pause fill yields.
    hover_active: Cell<bool>,
    /// Pause progressive reached cap / no candidates — never resumes until clear.
    progressive_done: Cell<bool>,
}

impl ThumbSink {
    /// Create a sink on an opened video track (decoder lazy on first decode).
    pub fn new(track: Rc<Mp4VideoTrack>, opts: ThumbSinkOptions) -> Self {
        let max_w = opts.max_w.max(1);
        let max_h = opts.max_h.max(1);
        Self {
            track,
            cache: RefCell::new(ThumbCache::new(opts.max_gops, opts.max_thumbs)),
            epoch: Cell::new(0),
            decoder: RefCell::new(None),
            blit: BlitCanvasSlot::default(),
            decode_blocked: Cell::new(false),
            flight: SingleFlight::new(),
            pending_aim: Cell::new(None),
            hover_aim: Cell::new(0),
            hover_dir: Cell::new(0),
            prev_hover_aim: Cell::new(None),
            last_max_w: Cell::new(max_w),
            last_max_h: Cell::new(max_h),
            last_aim_decode_ms: Cell::new(0.0),
            hover_active: Cell::new(false),
            progressive_done: Cell::new(false),
        }
    }

    pub fn track(&self) -> &Rc<Mp4VideoTrack> {
        &self.track
    }

    pub fn cancel(&self) {
        self.epoch.set(self.epoch.get().wrapping_add(1));
        self.pending_aim.set(None);
        self.clear_hover_direction();
        self.hover_active.set(false);
    }

    pub fn set_hover_active(&self, active: bool) {
        self.hover_active.set(active);
    }

    pub fn hover_active(&self) -> bool {
        self.hover_active.get()
    }

    pub fn progressive_done(&self) -> bool {
        self.progressive_done.get()
    }

    pub fn mark_progressive_done(&self) {
        self.progressive_done.set(true);
    }

    pub fn generation(&self) -> u64 {
        self.epoch.get()
    }

    pub fn decode_blocked(&self) -> bool {
        self.decode_blocked.get()
    }

    pub fn set_decode_blocked(&self, blocked: bool) {
        self.decode_blocked.set(blocked);
    }

    pub fn is_busy(&self) -> bool {
        self.flight.is_busy()
    }

    pub fn note_size(&self, max_w: u32, max_h: u32) {
        self.last_max_w.set(max_w.max(1));
        self.last_max_h.set(max_h.max(1));
    }

    pub fn last_size(&self) -> (u32, u32) {
        (self.last_max_w.get(), self.last_max_h.get())
    }

    pub fn set_fill_pending(&self, pending: bool) {
        self.flight.set_pending(pending);
    }

    pub fn take_fill_pending(&self) -> bool {
        self.flight.take_pending()
    }

    pub fn last_aim_decode_ms(&self) -> f64 {
        self.last_aim_decode_ms.get()
    }

    pub fn note_aim_decode_ms(&self, now_ms: f64) {
        self.last_aim_decode_ms.set(now_ms);
    }

    /// Drop cache + decoder (open / close / `ThumbPolicy::Off`).
    pub fn clear(&self) {
        self.cancel();
        if let Ok(mut cache) = self.cache.try_borrow_mut() {
            cache.clear();
        }
        if let Ok(mut slot) = self.decoder.try_borrow_mut()
            && let Some(mut d) = slot.take()
        {
            let _ = d.reset();
        }
        self.decode_blocked.set(false);
        self.flight.release_busy();
        self.flight.clear_pending();
        self.progressive_done.set(false);
    }

    pub fn thumb_count(&self) -> usize {
        self.cache
            .try_borrow()
            .map(|c| c.thumb_count())
            .unwrap_or(0)
    }

    pub fn max_thumbs(&self) -> usize {
        self.cache.try_borrow().map(|c| c.max_thumbs()).unwrap_or(1)
    }

    /// Cache full for another sync-key insert (progressive stops here forever).
    pub fn at_capacity(&self) -> bool {
        self.cache
            .try_borrow()
            .map(|c| c.at_capacity())
            .unwrap_or(true)
    }

    pub fn store_key(&self, frame: FrameId) -> FrameId {
        self.track.index().nearest_keyframe(frame).unwrap_or(frame)
    }

    pub fn contains(&self, key: FrameId, frame: FrameId, width: u32, height: u32) -> bool {
        let Ok(cache) = self.cache.try_borrow() else {
            return false;
        };
        cache.contains(key, frame, width, height)
    }

    /// Sync cache hit at current thumb size (`mediabunny:` present cached canvas).
    pub fn try_get(&self, frame: FrameId) -> bool {
        let meta = self.track.metadata();
        let (tw, th) = self.thumb_dims(meta.width, meta.height);
        let key = self.store_key(frame);
        self.contains(key, frame, tw, th)
    }

    /// Sized thumb box for current [`Self::last_size`] and source display size.
    pub fn thumb_dims(&self, src_w: u32, src_h: u32) -> (u32, u32) {
        let (max_w, max_h) = self.last_size();
        hover_thumb_dims(src_w, src_h, max_w, max_h)
    }

    /// Nearest sync key missing from the cache at the current thumb size.
    pub fn next_missing_key(
        &self,
        keys: &[FrameId],
        focus: FrameId,
        src_w: u32,
        src_h: u32,
    ) -> Option<FrameId> {
        let (tw, th) = self.thumb_dims(src_w, src_h);
        next_missing_sync_key(keys, focus, |k| self.contains(k, k, tw, th))
    }

    /// Progressive rank for `frame` (sync key → grid rank; Exact non-key → max).
    pub fn progressive_rank_for(&self, frame: FrameId) -> u16 {
        let keys = self.track.index().sync_frame_ids();
        let duration_sec = self.track.metadata().duration().seconds;
        let coarse = thumb_progressive_coarse_count(duration_sec, keys.len(), self.max_thumbs());
        let store_key = self.store_key(frame);
        if frame != store_key {
            return u16::MAX;
        }
        thumb_progressive_rank(&keys, store_key, coarse)
    }

    /// Next Pause progressive key (coarse → farthest), or `None` if done / cannot
    /// admit without dropping a coarser rank (marks [`Self::progressive_done`]).
    pub fn next_progressive_key(
        &self,
        keys: &[FrameId],
        src_w: u32,
        src_h: u32,
    ) -> Option<FrameId> {
        if self.progressive_done() {
            return None;
        }
        let (tw, th) = self.thumb_dims(src_w, src_h);
        let duration_sec = self.track.metadata().duration().seconds;
        let coarse = thumb_progressive_coarse_count(duration_sec, keys.len(), self.max_thumbs());
        let next = next_progressive_sync_key(keys, |k| self.contains(k, k, tw, th), coarse);
        let Some(k) = next else {
            self.mark_progressive_done();
            return None;
        };
        let needs_new_gop = !self.has_gop(k);
        let rank = thumb_progressive_rank(keys, k, coarse);
        if !self.can_admit(rank, needs_new_gop) {
            self.mark_progressive_done();
            return None;
        }
        Some(k)
    }

    pub fn has_gop(&self, key: FrameId) -> bool {
        self.cache
            .try_borrow()
            .map(|c| c.has_gop(key))
            .unwrap_or(false)
    }

    pub fn can_admit(&self, rank: u16, needs_new_gop: bool) -> bool {
        self.cache
            .try_borrow()
            .map(|c| c.can_admit(rank, needs_new_gop))
            .unwrap_or(false)
    }

    /// Lookup + paint while the cache borrow is held (`ImageBitmap` is not `Clone`).
    pub fn with_bitmap<R>(
        &self,
        key: FrameId,
        frame: FrameId,
        width: u32,
        height: u32,
        f: impl FnOnce(&ImageBitmap) -> R,
    ) -> Option<R> {
        let Ok(mut cache) = self.cache.try_borrow_mut() else {
            return None;
        };
        cache.get(key, frame, width, height).map(f)
    }

    /// Cached thumb neighbor biased by `dir` (`scrub_preview_neighbor`).
    pub fn nearest(&self, target: FrameId, dir: i8) -> Option<FrameId> {
        let meta = self.track.metadata();
        let (tw, th) = self.thumb_dims(meta.width, meta.height);
        self.nearest_frame_with_dir(target, tw, th, dir)
    }

    pub fn nearest_frame_with_dir(
        &self,
        target: FrameId,
        width: u32,
        height: u32,
        dir: i8,
    ) -> Option<FrameId> {
        let Ok(cache) = self.cache.try_borrow() else {
            return None;
        };
        cache
            .nearest_frame_with_dir(target, width, height, dir)
            .map(|(_, f)| f)
    }

    /// Paint directional nearest thumb; returns the painted frame id.
    pub fn with_nearest_bitmap_dir<R>(
        &self,
        target: FrameId,
        width: u32,
        height: u32,
        dir: i8,
        f: impl FnOnce(&ImageBitmap) -> R,
    ) -> Option<(FrameId, R)> {
        let Ok(mut cache) = self.cache.try_borrow_mut() else {
            return None;
        };
        let (key, frame) = cache.nearest_frame_with_dir(target, width, height, dir)?;
        let out = cache.get(key, frame, width, height).map(f)?;
        Some((frame, out))
    }

    pub fn put_ready(
        &self,
        key: FrameId,
        frame: FrameId,
        bitmap: ImageBitmap,
        width: u32,
        height: u32,
    ) {
        let rank = self.progressive_rank_for(frame);
        if let Ok(mut cache) = self.cache.try_borrow_mut() {
            cache.put_ready(key, frame, bitmap, width, height, rank);
        } else {
            bitmap.close();
        }
    }

    /// Draw a cached thumb into `dest` when present.
    pub fn paint_to(
        &self,
        frame: FrameId,
        dest: &HtmlCanvasElement,
    ) -> Result<Option<FrameId>, MediaError> {
        let meta = self.track.metadata();
        let (tw, th) = self.thumb_dims(meta.width, meta.height);
        let key = self.store_key(frame);
        match self.with_bitmap(key, frame, tw, th, |bmp| {
            paint_thumb_canvas(dest, bmp, tw, th)
        }) {
            Some(Ok(())) => Ok(Some(frame)),
            Some(Err(e)) => Err(e),
            None => Ok(None),
        }
    }

    // —— Hover client session (VideoPlayer / `<HoverThumb>`; not MB CanvasSink) ——

    pub fn set_hover_aim(&self, aim: FrameId) {
        self.hover_active.set(true);
        if let Some(prev) = self.prev_hover_aim.get() {
            if aim > prev {
                self.hover_dir.set(1);
            } else if aim < prev {
                self.hover_dir.set(-1);
            }
        }
        self.prev_hover_aim.set(Some(aim));
        self.hover_aim.set(aim);
    }

    pub fn hover_aim(&self) -> FrameId {
        self.hover_aim.get()
    }

    pub fn hover_dir(&self) -> i8 {
        self.hover_dir.get()
    }

    pub fn clear_hover_direction(&self) {
        self.hover_dir.set(0);
        self.prev_hover_aim.set(None);
    }

    pub fn take_decoder(&self) -> Option<WebCodecsDecoder> {
        self.decoder.borrow_mut().take()
    }

    pub fn put_decoder(&self, decoder: WebCodecsDecoder) {
        *self.decoder.borrow_mut() = Some(decoder);
    }

    pub fn with_blit<R>(
        &self,
        f: impl FnOnce(&BlitCanvas) -> Result<R, MediaError>,
    ) -> Result<R, MediaError> {
        self.blit.with_blit(f)
    }

    /// Record latest aim; return `true` if this caller should run the decode loop.
    pub fn claim_decode(&self, aim: FrameId) -> bool {
        self.hover_aim.set(aim);
        self.pending_aim.set(Some(aim));
        self.flight.try_claim()
    }

    /// Bg fill: claim worker without stomping [`Self::hover_aim`].
    pub fn claim_fill_key(&self, key: FrameId) -> bool {
        if !self.flight.request() {
            return false;
        }
        self.pending_aim.set(Some(key));
        true
    }

    pub fn take_pending_aim(&self) -> Option<FrameId> {
        self.pending_aim.take()
    }

    pub fn has_pending_aim(&self) -> bool {
        self.pending_aim.get().is_some()
    }

    /// Queue next bg key while the fill worker stays busy.
    pub fn queue_fill_key(&self, key: FrameId) {
        self.pending_aim.set(Some(key));
    }

    pub fn end_decode(&self) {
        self.flight.release_busy();
    }

    /// Decode `frame` into the cache. Caller must own the decode flight (or be
    /// sole user). Prefer [`Self::get`] for the public single-shot API.
    pub async fn decode_into_cache(
        &self,
        frame: FrameId,
        cancel: &dyn Fn() -> bool,
    ) -> Result<FrameId, MediaError> {
        if cancel() {
            return Err(MediaError::Cancelled);
        }
        let meta = self.track.metadata();
        let count = meta.frame_count;
        if frame >= count {
            return Err(MediaError::FrameOutOfRange(frame, count));
        }
        let (tw, th) = self.thumb_dims(meta.width, meta.height);
        let store_key = self.store_key(frame);
        if self.contains(store_key, frame, tw, th) {
            return Ok(frame);
        }
        if self.decode_blocked.get() {
            return Err(MediaError::Cancelled);
        }

        let taken = self.take_decoder();
        let mut decoder = match taken {
            Some(d) => d,
            None => match WebCodecsDecoder::create_thumb(&meta).await {
                Ok(d) => d,
                Err(MediaError::CodecUnsupported(c)) => {
                    self.set_decode_blocked(true);
                    return Err(MediaError::CodecUnsupported(c));
                }
                Err(e) => return Err(e),
            },
        };
        if cancel() {
            self.put_decoder(decoder);
            return Err(MediaError::Cancelled);
        }

        let decoded = decoder.decode_frame(&self.track, frame, cancel).await;
        let video = match decoded {
            Ok(v) => {
                self.put_decoder(decoder);
                v
            }
            Err(e) => {
                let _ = decoder.reset();
                self.put_decoder(decoder);
                return Err(e);
            }
        };
        if cancel() {
            drop(video);
            return Err(MediaError::Cancelled);
        }

        let thumb = self.with_blit(|enc| {
            enc.frame_to_bitmap(video.as_video_frame(), tw, th)
                .map(|(bmp, _, _)| bmp)
        })?;
        drop(video);
        if cancel() {
            thumb.close();
            return Err(MediaError::Cancelled);
        }
        self.put_ready(store_key, frame, thumb, tw, th);
        Ok(frame)
    }

    /// Ensure a sized thumb for `frame` is in cache (`mediabunny:` `getCanvas`).
    ///
    /// Returns `Ok(None)` when the codec cannot drive a 3rd decoder (Exact-only).
    /// Claims the decode flight — do not call while a fill/hover worker holds it.
    pub async fn get(
        &self,
        frame: FrameId,
        cancel: &dyn Fn() -> bool,
    ) -> Result<Option<FrameId>, MediaError> {
        if cancel() {
            return Err(MediaError::Cancelled);
        }
        let meta = self.track.metadata();
        let (tw, th) = self.thumb_dims(meta.width, meta.height);
        let store_key = self.store_key(frame);
        if self.contains(store_key, frame, tw, th) {
            return Ok(Some(frame));
        }
        if self.decode_blocked.get() {
            return Ok(None);
        }
        if !self.flight.try_claim() {
            return Err(MediaError::Cancelled);
        }
        let result = self.decode_into_cache(frame, cancel).await;
        self.end_decode();
        match result {
            Ok(id) => Ok(Some(id)),
            Err(MediaError::CodecUnsupported(_)) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Sparse FrameIds → cache (`mediabunny:` `canvasesAtTimestamps`).
    ///
    /// Decodes sequentially; pass monotonically increasing ids when possible.
    /// Each slot is `Some(frame)` when ready, `None` when unsupported/skip.
    pub async fn at_frames(
        &self,
        frames: &[FrameId],
        cancel: &dyn Fn() -> bool,
    ) -> Result<Vec<Option<FrameId>>, MediaError> {
        let mut out = Vec::with_capacity(frames.len());
        for &frame in frames {
            if cancel() {
                return Err(MediaError::Cancelled);
            }
            out.push(self.get(frame, cancel).await?);
        }
        Ok(out)
    }
}

/// Draw a sized thumb bitmap into `dest` (hover-thumb canvas).
pub fn paint_thumb_canvas(
    dest: &HtmlCanvasElement,
    bitmap: &ImageBitmap,
    width: u32,
    height: u32,
) -> Result<(), MediaError> {
    let width = width.max(1);
    let height = height.max(1);
    if dest.width() != width {
        dest.set_width(width);
    }
    if dest.height() != height {
        dest.set_height(height);
    }
    let ctx = canvas_2d(dest)?;
    ctx.set_image_smoothing_enabled(true);
    ctx.draw_image_with_image_bitmap_and_dw_and_dh(bitmap, 0.0, 0.0, width as f64, height as f64)
        .map_err(MediaError::from)?;
    Ok(())
}

/// Sparse keyframe → ThumbCache (`mediabunny:` `canvasesAtTimestamps`).
///
/// `next_gap` returns the next progressive sync id. Yields on Play / hover /
/// pending hover aim. Stops when `next_gap` is `None` (cap or done).
pub async fn run_sparse_key_fill(
    sink: Rc<ThumbSink>,
    mut next_gap: impl FnMut() -> Option<FrameId>,
    yield_to_play: impl Fn() -> bool,
) -> Result<(), MediaError> {
    let end = || sink.end_decode();
    let should_yield = || {
        yield_to_play() || sink.decode_blocked() || sink.hover_active() || sink.progressive_done()
    };

    loop {
        if should_yield() {
            end();
            return Ok(());
        }
        let Some(key) = sink.take_pending_aim() else {
            end();
            return Ok(());
        };

        let meta = sink.track().metadata();
        let (tw, th) = sink.thumb_dims(meta.width, meta.height);
        if !sink.contains(key, key, tw, th) {
            let rank = sink.progressive_rank_for(key);
            let needs_new_gop = !sink.has_gop(key);
            if !sink.can_admit(rank, needs_new_gop) {
                sink.mark_progressive_done();
                end();
                return Ok(());
            }
            let cancelled = || should_yield() || sink.has_pending_aim() || sink.hover_active();
            if cancelled() {
                end();
                return Ok(());
            }
            match sink.decode_into_cache(key, &cancelled).await {
                Ok(_) => {}
                Err(MediaError::Cancelled) | Err(MediaError::CodecUnsupported(_)) => {
                    end();
                    return Ok(());
                }
                Err(e) => {
                    end();
                    return Err(e);
                }
            }
        }

        if should_yield() {
            end();
            return Ok(());
        }

        // Hover queued mid-fill — release so paint_hover can claim.
        if sink.has_pending_aim() {
            end();
            return Ok(());
        }

        if let Some(k) = next_gap() {
            sink.queue_fill_key(k);
            continue;
        }

        let want_again = sink.take_fill_pending();
        end();
        if want_again
            && !should_yield()
            && let Some(k) = next_gap()
            && sink.claim_fill_key(k)
        {
            continue;
        }
        return Ok(());
    }
}
