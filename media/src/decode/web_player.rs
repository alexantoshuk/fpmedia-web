//! High-level browser player: demux + WebCodecs + canvas.
//!
//! Present and decode share the player via interior mutability so an rAF
//! present loop never waits on `prefetch_one` (mediabunny-style split).

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::rc::Rc;

use web_sys::{CanvasRenderingContext2d, HtmlCanvasElement, VideoFrame};

use crate::decode::canvas::{canvas_2d, configure_canvas_for_video, draw_frame_with_context};
use crate::decode::frame_cache::{
    DenseFrameCache, ThumbCache, DEFAULT_MAX_CHUNKS, DEFAULT_MAX_FRAMES, DEFAULT_MAX_THUMBS,
    DEFAULT_MAX_THUMB_GOPS,
};
use crate::decode::webcodecs::WebCodecsDecoder;
use crate::error::MediaError;
use crate::player::{Player, PlayerState};
use crate::types::{FrameId, VideoMetadata};

/// Decoded frames held ready for canvas present (mediabunny-style prefetch).
pub const PRESENTATION_QUEUE_CAP: usize = 16;
/// Frames to decode before starting the media clock (rest filled by prefetch).
const PLAY_PRIME_FRAMES: usize = 2;
/// Start-of-clip frames prepared into `loop_prime` before EOF (one decoder).
const LOOP_PRIME_FRAMES: usize = 8;
/// Wall-clock lag above this → skip to nearest keyframe instead of exact frame.
const BIG_LAG_SECONDS: f64 = 0.5;
/// Stop prefetching this many ms before the present deadline.
const PREFETCH_DEADLINE_SLACK_MS: f64 = 3.0;

struct QueuedFrame {
    frame: FrameId,
    video: VideoFrame,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FillBias {
    Behind,
    Ahead,
}

struct Shared {
    player: RefCell<Player>,
    presentation: RefCell<VecDeque<QueuedFrame>>,
    /// Exact frames around the playhead (GOP chunks); replaces step_history.
    dense_cache: RefCell<DenseFrameCache>,
    /// Sparse scrub thumbs (caller-sized); separate budget from dense.
    thumb_cache: RefCell<ThumbCache>,
    /// Second decoder for background GOP fill (does not steal main playback).
    scrub_decoder: RefCell<Option<WebCodecsDecoder>>,
    scrub_epoch: Cell<u64>,
    scrub_busy: Cell<bool>,
    fill_bias: Cell<FillBias>,
    /// Start frames decoded ahead of the loop seam (same decoder, while end plays).
    loop_prime: RefCell<Option<VecDeque<QueuedFrame>>>,
    canvas: HtmlCanvasElement,
    ctx: CanvasRenderingContext2d,
    frames_skipped: Cell<u64>,
    last_behind_ms: Cell<Option<f64>>,
    /// Prefetch must idle while seek/loop prepare owns the decoder.
    restarting: Cell<bool>,
    preparing_loop: Cell<bool>,
    /// Bumped to cancel in-flight decode (mediabunny dispose / asyncId).
    op_epoch: Cell<u64>,
    /// Set when invalidate runs while the decoder slot is empty (checked out).
    /// Honored on the next `take_decoder` put-back / decode start.
    decoder_reset: Cell<bool>,
}

/// Live decode / prefetch counters for the demo UI (and host apps).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PipelineStats {
    pub presentation_len: usize,
    pub presentation_cap: usize,
    pub presentation_from: Option<FrameId>,
    pub presentation_to: Option<FrameId>,
    pub held_len: usize,
    pub decode_queue_size: u32,
    pub frames_skipped: u64,
    pub last_behind_ms: Option<f64>,
    pub cache_chunks: usize,
    pub cache_frames: usize,
    pub scrub_busy: bool,
}

/// Result of a sync clock-driven present attempt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DuePresent {
    Playing { did_draw: bool },
    NeedsLoop,
    Ended,
}

/// Frame-accurate player that decodes with WebCodecs and draws to a canvas.
pub struct WebPlayer {
    shared: Rc<Shared>,
    /// `None` only while an async decode owns the decoder (must not re-enter).
    decoder: RefCell<Option<WebCodecsDecoder>>,
}

impl WebPlayer {
    pub async fn open_mp4_bytes(
        bytes: Vec<u8>,
        canvas: HtmlCanvasElement,
    ) -> Result<Self, MediaError> {
        let player = Player::open_mp4_bytes(bytes)?;
        Self::from_player(player, canvas).await
    }

    /// Progressive open from a ranged [`crate::MediaSource`] (Blob / memory).
    pub async fn open_mp4_source(
        source: std::sync::Arc<dyn crate::MediaSource>,
        canvas: HtmlCanvasElement,
    ) -> Result<Self, MediaError> {
        let track = crate::open_mp4_source_cached(
            source,
            crate::OpenMp4Options {
                verify_avc_sync: true,
            },
        )
        .await?;
        Self::from_track(track, canvas).await
    }

    /// Progressive open from a browser [`web_sys::Blob`] / `File`.
    pub async fn open_blob(blob: web_sys::Blob, canvas: HtmlCanvasElement) -> Result<Self, MediaError> {
        let source: std::sync::Arc<dyn crate::MediaSource> =
            std::sync::Arc::new(crate::BlobSource::new(blob));
        Self::open_mp4_source(source, canvas).await
    }

    /// Build a player around an already-demuxed track (host can show stage status).
    pub async fn from_track(
        track: crate::demux::Mp4VideoTrack,
        canvas: HtmlCanvasElement,
    ) -> Result<Self, MediaError> {
        let player = Player::from_track(track)?;
        Self::from_player(player, canvas).await
    }

    async fn from_player(player: Player, canvas: HtmlCanvasElement) -> Result<Self, MediaError> {
        let meta = player.metadata().clone();
        configure_canvas_for_video(&canvas, meta.width, meta.height)?;
        let decoder = WebCodecsDecoder::create(&meta).await?;
        let ctx = canvas_2d(&canvas)?;
        let shared = Rc::new(Shared {
            player: RefCell::new(player),
            presentation: RefCell::new(VecDeque::new()),
            dense_cache: RefCell::new(DenseFrameCache::new(
                DEFAULT_MAX_CHUNKS,
                DEFAULT_MAX_FRAMES,
            )),
            thumb_cache: RefCell::new(ThumbCache::new(
                DEFAULT_MAX_THUMB_GOPS,
                DEFAULT_MAX_THUMBS,
            )),
            scrub_decoder: RefCell::new(None),
            scrub_epoch: Cell::new(0),
            scrub_busy: Cell::new(false),
            fill_bias: Cell::new(FillBias::Ahead),
            loop_prime: RefCell::new(None),
            canvas,
            ctx,
            frames_skipped: Cell::new(0),
            last_behind_ms: Cell::new(None),
            restarting: Cell::new(false),
            preparing_loop: Cell::new(false),
            op_epoch: Cell::new(0),
            decoder_reset: Cell::new(false),
        });
        // Do not seek here — first-frame decode can be slow; let the host show
        // metadata first and call `seek_frame(0)` with its own progress UI.
        Ok(Self {
            shared,
            decoder: RefCell::new(Some(decoder)),
        })
    }

    pub fn metadata(&self) -> VideoMetadata {
        self.shared.player.borrow().metadata().clone()
    }

    pub fn state(&self) -> PlayerState {
        self.shared.player.borrow().state()
    }

    pub fn canvas(&self) -> &HtmlCanvasElement {
        &self.shared.canvas
    }

    /// Recompute canvas buffer from current CSS layout × DPR (call on container resize).
    pub fn relayout(&self) -> Result<(), MediaError> {
        let meta = self.metadata();
        configure_canvas_for_video(&self.shared.canvas, meta.width, meta.height)
    }

    pub fn is_playing(&self) -> bool {
        self.state().playing
    }

    pub fn play(&self) {
        self.shared.fill_bias.set(FillBias::Ahead);
        // WASM is single-threaded — cancel scrub GOP fill so it cannot steal
        // the event loop from present/prefetch (caused multi-second lag +
        // main-decoder flush/idle misses under skip-behind).
        let scrub = self.shared.scrub_epoch.get().wrapping_add(1);
        self.shared.scrub_epoch.set(scrub);
        self.shared.player.borrow_mut().play();
        self.shared.frames_skipped.set(0);
        self.shared.last_behind_ms.set(None);
    }

    pub fn pause(&self) {
        // Invalidate in-flight seek/prefetch (mediabunny-style cancel).
        self.cancel_ops();
        self.shared.player.borrow_mut().pause();
        self.clear_loop_prime();
        // Fill GOP window for fast PREV while idle.
        self.schedule_ensure_window();
    }

    /// Bump the op epoch so in-flight decode/seek ignores its result.
    pub fn cancel_ops(&self) {
        let next = self.shared.op_epoch.get().wrapping_add(1);
        self.shared.op_epoch.set(next);
        let scrub = self.shared.scrub_epoch.get().wrapping_add(1);
        self.shared.scrub_epoch.set(scrub);
    }

    fn begin_op(&self) -> u64 {
        self.cancel_ops();
        self.shared.op_epoch.get()
    }

    fn is_current(&self, epoch: u64) -> bool {
        self.shared.op_epoch.get() == epoch
    }

    pub fn set_loop(&self, looping: bool) {
        self.shared.player.borrow_mut().set_loop(looping);
        if !looping {
            self.clear_loop_prime();
        }
    }

    pub fn is_looping(&self) -> bool {
        self.shared.player.borrow().is_looping()
    }

    pub fn is_restarting(&self) -> bool {
        self.shared.restarting.get()
    }

    pub fn pipeline_stats(&self) -> PipelineStats {
        let presentation = self.shared.presentation.borrow();
        let cache = self.shared.dense_cache.borrow();
        // While decode owns the decoder (`None` in the slot), report busy.
        let (held_len, decode_queue_size) = match self.decoder.borrow().as_ref() {
            Some(d) => (d.held_len(), d.browser_decode_queue_size()),
            None => (usize::MAX, u32::MAX),
        };
        PipelineStats {
            presentation_len: presentation.len(),
            presentation_cap: PRESENTATION_QUEUE_CAP,
            presentation_from: presentation.front().map(|q| q.frame),
            presentation_to: presentation.back().map(|q| q.frame),
            held_len,
            decode_queue_size,
            frames_skipped: self.shared.frames_skipped.get(),
            last_behind_ms: self.shared.last_behind_ms.get(),
            cache_chunks: cache.chunk_count(),
            cache_frames: cache.frame_count(),
            scrub_busy: self.shared.scrub_busy.get(),
        }
    }

    pub fn frame_duration_seconds(&self) -> Result<f64, MediaError> {
        self.duration_of(self.state().frame)
    }

    fn duration_of(&self, frame: FrameId) -> Result<f64, MediaError> {
        let player = self.shared.player.borrow();
        let entry = player
            .track()
            .index()
            .get(frame)
            .ok_or(MediaError::FrameOutOfRange(frame, player.state().frame_count))?;
        if entry.duration_ticks == 0 || entry.timescale == 0 {
            return Err(MediaError::MissingFrameDuration(frame));
        }
        Ok(entry.duration_ticks as f64 / entry.timescale as f64)
    }

    pub async fn seek_frame(&self, frame: FrameId) -> Result<(), MediaError> {
        let epoch = self.begin_op();
        self.shared.restarting.set(true);
        let result = async {
            self.shared.player.borrow_mut().pause();
            self.clear_loop_prime();
            self.clear_presentation();
            self.clear_dense_cache();
            self.invalidate_decoder();
            self.goto_frame(frame, epoch).await
        }
        .await;
        self.shared.restarting.set(false);
        if !self.is_current(epoch) {
            return Err(MediaError::Cancelled);
        }
        result?;
        self.schedule_ensure_window();
        Ok(())
    }

    pub async fn next_frame(&self) -> Result<(), MediaError> {
        let epoch = self.begin_op();
        self.shared.player.borrow_mut().pause();
        self.shared.fill_bias.set(FillBias::Ahead);
        let next = self.state().frame.saturating_add(1);
        // Reuse prefetch when present — clearing the queue would close the only
        // copy of frames the warm decoder already emitted.
        {
            let mut presentation = self.shared.presentation.borrow_mut();
            if presentation.front().is_some_and(|f| f.frame == next) {
                let queued = presentation.pop_front().expect("front checked");
                drop(presentation);
                self.present_and_remember(queued.frame, queued.video)?;
                self.shared.player.borrow_mut().commit_frame(queued.frame)?;
                return Ok(());
            }
        }
        // Instant next from dense cache (e.g. after stepping back).
        if let Some(video) = self.clone_from_dense_cache(next) {
            self.clear_presentation();
            self.present_only(video)?;
            self.shared.player.borrow_mut().commit_frame(next)?;
            self.touch_playhead(next);
            self.schedule_ensure_window();
            return Ok(());
        }
        self.clear_presentation();
        self.goto_frame(next, epoch).await?;
        self.schedule_ensure_window();
        Ok(())
    }

    pub async fn previous_frame(&self) -> Result<(), MediaError> {
        let epoch = self.begin_op();
        self.shared.player.borrow_mut().pause();
        self.shared.fill_bias.set(FillBias::Behind);
        let cur = self.state().frame;
        if cur == 0 {
            return Err(MediaError::FrameOutOfRange(0, self.state().frame_count));
        }
        let prev = cur - 1;
        // Prefer dense cache — key→N cascade is multi-second on long GOPs.
        if let Some(video) = self.clone_from_dense_cache(prev) {
            self.clear_presentation();
            // Keep the warm decoder; only a cache miss needs invalidate.
            self.present_only(video)?;
            self.shared.player.borrow_mut().commit_frame(prev)?;
            self.touch_playhead(prev);
            self.schedule_ensure_window();
            return Ok(());
        }
        self.clear_presentation();
        self.invalidate_decoder();
        self.goto_frame(prev, epoch).await?;
        self.schedule_ensure_window();
        Ok(())
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
                self.present_and_remember(queued.frame, queued.video)?;
                self.shared.player.borrow_mut().commit_frame(queued.frame)?;
                return Ok(self.has_more_playback());
            }
        }

        self.clear_presentation();
        if behind_seconds >= BIG_LAG_SECONDS {
            self.clear_dense_cache();
            self.invalidate_decoder();
        }
        let epoch = self.shared.op_epoch.get();
        self.goto_frame(target, epoch).await?;
        Ok(self.has_more_playback())
    }

    async fn end_or_loop(&self) -> Result<bool, MediaError> {
        if !self.is_looping() {
            self.shared.player.borrow_mut().pause();
            self.clear_presentation();
            return Ok(false);
        }
        self.clear_presentation();
        self.invalidate_decoder();
        let epoch = self.shared.op_epoch.get();
        self.goto_frame(0, epoch).await?;
        Ok(true)
    }

    fn has_more_playback(&self) -> bool {
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
        self.fill_presentation_queue(epoch).await
    }

    pub fn frame_timestamp_seconds(&self, frame: FrameId) -> Result<f64, MediaError> {
        let player = self.shared.player.borrow();
        let entry = player
            .track()
            .index()
            .get(frame)
            .ok_or(MediaError::FrameOutOfRange(frame, player.state().frame_count))?;
        Ok(entry.composition_timestamp().seconds)
    }

    pub fn presentation_len(&self) -> usize {
        self.shared.presentation.borrow().len()
    }

    /// True when the presentation queue already holds through the last frame.
    fn presentation_reaches_eof(&self) -> bool {
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
            let last = player
                .track()
                .index()
                .get(count - 1)
                .ok_or(MediaError::FrameOutOfRange(count - 1, count))?;
            last.composition_timestamp().seconds
                + (last.duration_ticks as f64 / last.timescale.max(1) as f64)
        };
        if media_time_secs >= last_end {
            if self.is_looping() {
                return Ok(DuePresent::NeedsLoop);
            }
            self.shared.player.borrow_mut().pause();
            self.clear_presentation();
            return Ok(DuePresent::Ended);
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
                queued.video.close();
                self.shared.player.borrow_mut().commit_frame(queued.frame)?;
                continue;
            }

            let lag_ms = ((media_time_secs - start) * 1000.0).max(0.0);
            if lag_ms > 1.0 {
                self.shared.last_behind_ms.set(Some(lag_ms));
            }
            self.present_and_remember(queued.frame, queued.video)?;
            self.shared.player.borrow_mut().commit_frame(queued.frame)?;
            did_draw = true;
            break;
        }

        Ok(DuePresent::Playing { did_draw })
    }

    /// Apply a prepared loop buffer if ready; otherwise cold-restart at frame 0.
    ///
    /// Prefer [`Self::prepare_loop_if_needed`] while the end still plays (same
    /// decoder, end frames already queued) so this is only a buffer swap.
    pub async fn loop_to_start(&self) -> Result<(), MediaError> {
        let mut waits = 0_u32;
        while self.shared.preparing_loop.get() && waits < 500 {
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
    pub async fn prepare_loop_if_needed(&self) -> Result<(), MediaError> {
        if !self.is_looping() || !self.is_playing() {
            return Ok(());
        }
        if self.shared.loop_prime.borrow().is_some() || self.shared.preparing_loop.get() {
            return Ok(());
        }
        if !self.presentation_reaches_eof() {
            return Ok(());
        }

        self.shared.preparing_loop.set(true);
        self.shared.restarting.set(true);
        let epoch = self.shared.op_epoch.get();
        let result = self.fill_loop_prime(epoch).await;
        self.shared.restarting.set(false);
        self.shared.preparing_loop.set(false);
        result
    }

    fn apply_loop_prime(&self) -> Result<bool, MediaError> {
        let Some(mut buf) = self.shared.loop_prime.borrow_mut().take() else {
            return Ok(false);
        };
        let Some(first) = buf.pop_front() else {
            return Ok(false);
        };

        self.clear_presentation();
        self.clear_dense_cache();
        self.present_and_remember(first.frame, first.video)?;
        self.shared.player.borrow_mut().commit_frame(first.frame)?;
        *self.shared.presentation.borrow_mut() = buf;
        // Decoder is already warm past the primed start frames.
        Ok(true)
    }

    async fn fill_loop_prime(&self, epoch: u64) -> Result<(), MediaError> {
        self.invalidate_decoder();
        let track = self.shared.player.borrow().track_rc();
        let count = self.state().frame_count;
        let n = (LOOP_PRIME_FRAMES as FrameId).clamp(1, count.max(1));
        let mut buf: VecDeque<QueuedFrame> = VecDeque::new();

        for frame in 0..n {
            if !self.is_playing() || !self.is_looping() || !self.is_current(epoch) {
                for q in buf.drain(..) {
                    q.video.close();
                }
                return Ok(());
            }
            match self.decode_frame(track.as_ref(), frame, epoch).await {
                Ok(video) => buf.push_back(QueuedFrame { frame, video }),
                Err(MediaError::Cancelled) => {
                    for q in buf.drain(..) {
                        q.video.close();
                    }
                    return Ok(());
                }
                Err(e) => {
                    for q in buf.drain(..) {
                        q.video.close();
                    }
                    return Err(e);
                }
            }
        }

        if !self.is_current(epoch) {
            for q in buf.drain(..) {
                q.video.close();
            }
            return Ok(());
        }

        *self.shared.loop_prime.borrow_mut() = Some(buf);
        Ok(())
    }

    async fn loop_to_start_cold(&self) -> Result<(), MediaError> {
        let epoch = self.begin_op();
        self.shared.restarting.set(true);
        let result = async {
            self.clear_loop_prime();
            self.clear_presentation();
            self.clear_dense_cache();
            self.invalidate_decoder();
            self.goto_frame(0, epoch).await?;
            if self.is_playing() && self.is_current(epoch) {
                self.prime_presentation(LOOP_PRIME_FRAMES.saturating_sub(1), epoch)
                    .await?;
            }
            Ok(())
        }
        .await;
        self.shared.restarting.set(false);
        if !self.is_current(epoch) {
            return Err(MediaError::Cancelled);
        }
        result
    }

    /// Decode one frame into the queue. May run concurrently with [`Self::present_due`].
    ///
    /// No-ops while [`Self::is_restarting`] — the restart/prepare path owns the decoder and
    /// fills via [`Self::prime_presentation`] / [`Self::fill_loop_prime`].
    pub async fn prefetch_one(&self) -> Result<bool, MediaError> {
        if !self.is_playing() || self.shared.restarting.get() {
            return Ok(false);
        }
        let epoch = self.shared.op_epoch.get();
        self.enqueue_next_presentation_frame(epoch).await
    }

    /// Push the next presentation-order frame (ignores the restarting gate).
    async fn enqueue_next_presentation_frame(&self, epoch: u64) -> Result<bool, MediaError> {
        if self.presentation_len() >= PRESENTATION_QUEUE_CAP {
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
            (player.track_rc(), next)
        };
        let video = match self.decode_frame(track.as_ref(), next, epoch).await {
            Ok(v) => v,
            Err(MediaError::Cancelled) => return Ok(false),
            Err(e) => return Err(e),
        };
        if !self.is_playing() || !self.is_current(epoch) {
            video.close();
            return Ok(false);
        }
        self.shared
            .presentation
            .borrow_mut()
            .push_back(QueuedFrame {
                frame: next,
                video,
            });
        Ok(true)
    }

    pub async fn prefetch_until(&self, deadline_ms: f64) -> Result<usize, MediaError> {
        let mut decoded = 0usize;
        while self.is_playing() && self.presentation_len() < PRESENTATION_QUEUE_CAP {
            if let Some(now) = performance_now_ms() {
                if now + PREFETCH_DEADLINE_SLACK_MS >= deadline_ms {
                    break;
                }
            }
            if !self.prefetch_one().await? {
                break;
            }
            decoded += 1;
        }
        Ok(decoded)
    }

    fn catch_up_frame(&self, next: FrameId, behind_seconds: f64) -> Result<FrameId, MediaError> {
        let player = self.shared.player.borrow();
        let index = player.track().index();
        let next_entry = index
            .get(next)
            .ok_or(MediaError::FrameOutOfRange(next, player.state().frame_count))?;
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

        if behind_seconds >= BIG_LAG_SECONDS {
            if let Some(key) = index.nearest_keyframe(target) {
                if key >= next {
                    return Ok(key);
                }
            }
        }
        Ok(target)
    }

    async fn present_frame(&self, frame: FrameId) -> Result<(), MediaError> {
        {
            let mut presentation = self.shared.presentation.borrow_mut();
            if let Some(front) = presentation.front() {
                if front.frame == frame {
                    let queued = presentation.pop_front().expect("front checked");
                    drop(presentation);
                    self.present_and_remember(queued.frame, queued.video)?;
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
                        self.present_and_remember(queued.frame, queued.video)?;
                        self.shared.player.borrow_mut().commit_frame(queued.frame)?;
                        return Ok(());
                    }
                } else {
                    drop(presentation);
                    self.clear_presentation();
                }
            }
        }
        self.goto_frame(frame, self.shared.op_epoch.get()).await
    }

    async fn fill_presentation_queue(&self, epoch: u64) -> Result<(), MediaError> {
        // Start playback after a couple of frames; background prefetch fills to CAP.
        self.prime_presentation(PLAY_PRIME_FRAMES, epoch).await
    }

    /// Decode up to `min_frames` into the presentation queue (bypasses restarting gate).
    async fn prime_presentation(&self, min_frames: usize, epoch: u64) -> Result<(), MediaError> {
        let target = min_frames.min(PRESENTATION_QUEUE_CAP);
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

    fn drain_presentation_before(&self, frame: FrameId) {
        let mut presentation = self.shared.presentation.borrow_mut();
        while presentation.front().is_some_and(|f| f.frame < frame) {
            if let Some(q) = presentation.pop_front() {
                q.video.close();
            }
        }
    }

    fn clear_presentation(&self) {
        let mut presentation = self.shared.presentation.borrow_mut();
        while let Some(q) = presentation.pop_front() {
            q.video.close();
        }
    }

    fn clear_dense_cache(&self) {
        self.shared.dense_cache.borrow_mut().clear();
        self.shared.thumb_cache.borrow_mut().clear();
        // Abort in-flight scrub fill.
        let scrub = self.shared.scrub_epoch.get().wrapping_add(1);
        self.shared.scrub_epoch.set(scrub);
        if let Some(mut d) = self.shared.scrub_decoder.borrow_mut().take() {
            let _ = d.reset();
        }
    }

    fn key_for(&self, frame: FrameId) -> Option<FrameId> {
        self.shared
            .player
            .borrow()
            .track()
            .index()
            .nearest_keyframe(frame)
    }

    fn touch_playhead(&self, frame: FrameId) {
        if let Some(key) = self.key_for(frame) {
            self.shared
                .dense_cache
                .borrow_mut()
                .touch_playhead(frame, key);
        }
    }

    fn clone_from_dense_cache(&self, frame: FrameId) -> Option<VideoFrame> {
        let key = self.key_for(frame)?;
        self.shared.dense_cache.borrow_mut().get_clone(frame, key)
    }

    fn clear_loop_prime(&self) {
        if let Some(buf) = self.shared.loop_prime.borrow_mut().take() {
            for q in buf {
                q.video.close();
            }
        }
    }

    fn invalidate_decoder(&self) {
        // Soft clear is not enough: clearing `input_timestamps` while the browser
        // still holds in-flight chunks desyncs PTS remap → rare
        // `flush/idle without pts=… (held=0)` on rapid PREV.
        self.shared.decoder_reset.set(true);
        if let Some(decoder) = self.decoder.borrow_mut().as_mut() {
            if decoder.reset().is_ok() {
                self.shared.decoder_reset.set(false);
            }
        }
    }

    /// Take the decoder out of the slot for the duration of an async decode so
    /// we never hold `RefMut` across `.await` (avoids freezes / panics).
    ///
    /// Waits briefly if another op still holds the decoder (quick Pause→Play:
    /// cancelled prefetch must put the decoder back before prime can run).
    async fn decode_frame(
        &self,
        track: &crate::demux::Mp4VideoTrack,
        frame: FrameId,
        epoch: u64,
    ) -> Result<VideoFrame, MediaError> {
        let mut decoder = self.take_decoder(epoch).await?;
        if self.shared.decoder_reset.get() {
            decoder.reset()?;
            self.shared.decoder_reset.set(false);
        }
        let cancelled = || !self.is_current(epoch);
        let result = decoder.decode_frame(track, frame, &cancelled).await;
        let result = match result {
            // One hard reset + retry — orphan PTS after a raced invalidate.
            Err(MediaError::WebCodecs(msg))
                if msg.contains("flush/idle without pts=")
                    || msg.contains("exhausted GOP before pts=") =>
            {
                if cancelled() {
                    Err(MediaError::Cancelled)
                } else {
                    decoder.reset()?;
                    decoder.decode_frame(track, frame, &cancelled).await
                }
            }
            other => other,
        };
        if cancelled() {
            let _ = decoder.reset();
            *self.decoder.borrow_mut() = Some(decoder);
            if let Ok(video) = result {
                video.close();
            }
            return Err(MediaError::Cancelled);
        }
        *self.decoder.borrow_mut() = Some(decoder);
        result
    }

    async fn take_decoder(&self, epoch: u64) -> Result<WebCodecsDecoder, MediaError> {
        // ~2s of setTimeout(0) spins — enough for a cancelled peer to restore the slot.
        for _ in 0..120 {
            if !self.is_current(epoch) {
                return Err(MediaError::Cancelled);
            }
            if let Some(decoder) = self.decoder.borrow_mut().take() {
                return Ok(decoder);
            }
            yield_event_loop().await;
        }
        Err(MediaError::WebCodecs(
            "decoder busy (Pause/Play raced — try Play again)".into(),
        ))
    }

    async fn goto_frame(&self, frame: FrameId, epoch: u64) -> Result<(), MediaError> {
        let track = self.shared.player.borrow().track_rc();
        let video_frame = self.decode_frame(track.as_ref(), frame, epoch).await?;
        if !self.is_current(epoch) {
            video_frame.close();
            return Err(MediaError::Cancelled);
        }
        self.present_and_remember(frame, video_frame)?;
        self.shared.player.borrow_mut().commit_frame(frame)?;
        Ok(())
    }

    /// Draw, then **move** the frame into the dense GOP cache (no `VideoFrame.clone`).
    /// Safe on the play `present_due` path — clone-per-frame was what killed fps.
    fn present_and_remember(&self, frame_id: FrameId, video: VideoFrame) -> Result<(), MediaError> {
        let result = draw_frame_with_context(&self.shared.canvas, &self.shared.ctx, &video);
        if result.is_ok() {
            if let Some(key) = self.key_for(frame_id) {
                let mut cache = self.shared.dense_cache.borrow_mut();
                cache.insert_owned(frame_id, key, video);
                cache.touch_playhead(frame_id, key);
            } else {
                video.close();
            }
            // Play already streams frames into the cache; scrub fill only when paused.
            if !self.is_playing() {
                self.schedule_ensure_window();
            }
        } else {
            video.close();
        }
        result
    }

    /// Draw a cache clone (or one-shot frame) without parking another copy.
    fn present_only(&self, video: VideoFrame) -> Result<(), MediaError> {
        let result = draw_frame_with_context(&self.shared.canvas, &self.shared.ctx, &video);
        video.close();
        result
    }

    /// Exact frame from dense cache or scrub decode (clone / owned).
    pub async fn frame_at(&self, frame: FrameId) -> Result<VideoFrame, MediaError> {
        let count = self.state().frame_count;
        if frame >= count {
            return Err(MediaError::FrameOutOfRange(frame, count));
        }
        if let Some(video) = self.clone_from_dense_cache(frame) {
            return Ok(video);
        }
        let key = self
            .key_for(frame)
            .ok_or(MediaError::FrameOutOfRange(frame, count))?;
        self.scrub_fill_chunk(key).await?;
        self.clone_from_dense_cache(frame)
            .ok_or_else(|| MediaError::WebCodecs(format!("frame_at miss after scrub fill ({frame})")))
    }

    /// Sparse timestamps API — filmstrip follow-up (mediabunny `samplesAtTimestamps`).
    pub async fn frames_at_timestamps(
        &self,
        _timestamps: &[f64],
    ) -> Result<Vec<Option<VideoFrame>>, MediaError> {
        Err(MediaError::Unsupported(
            "frames_at_timestamps — use mediabunny samplesAtTimestamps pattern in a follow-up",
        ))
    }

    fn schedule_ensure_window(&self) {
        if self.is_playing()
            || self.shared.scrub_busy.get()
            || self.shared.restarting.get()
        {
            return;
        }
        self.shared.scrub_busy.set(true);
        let shared = Rc::clone(&self.shared);
        wasm_bindgen_futures::spawn_local(async move {
            let _ = ensure_window_task(shared).await;
        });
    }

    async fn scrub_fill_chunk(&self, key: FrameId) -> Result<(), MediaError> {
        let shared = Rc::clone(&self.shared);
        fill_one_chunk(shared, key).await
    }
}

impl Drop for WebPlayer {
    fn drop(&mut self) {
        self.clear_loop_prime();
        self.clear_presentation();
        self.clear_dense_cache();
    }
}

async fn ensure_window_task(shared: Rc<Shared>) -> Result<(), MediaError> {
    let scrub_epoch = shared.scrub_epoch.get();
    let result = async {
        if shared.restarting.get() || shared.player.borrow().state().playing {
            return Ok(());
        }
        let playhead = shared.player.borrow().state().frame;
        let index = shared.player.borrow().track().index().clone();
        let Some(playhead_key) = index.nearest_keyframe(playhead) else {
            return Ok(());
        };
        let prev_key = index.get(playhead_key).and_then(|e| {
            index
                .frames()
                .iter()
                .filter(|x| x.is_sync && x.decode_order < e.decode_order)
                .max_by_key(|x| x.decode_order)
                .map(|x| x.frame)
        });
        let next_key = index.next_keyframe_after(playhead_key);
        let behind_first = shared.fill_bias.get() == FillBias::Behind;
        let desired = DenseFrameCache::desired_chunk_keys(
            playhead_key,
            prev_key,
            next_key,
            behind_first,
        );
        let missing = {
            let cache = shared.dense_cache.borrow();
            cache.missing_among(&desired)
        };
        // At most one missing GOP per schedule — keep the UI responsive while paused.
        if let Some(key) = missing.first().copied() {
            if shared.scrub_epoch.get() != scrub_epoch
                || shared.restarting.get()
                || shared.player.borrow().state().playing
            {
                return Ok(());
            }
            fill_one_chunk(Rc::clone(&shared), key).await?;
        }
        Ok(())
    }
    .await;
    shared.scrub_busy.set(false);
    // If more chunks are still missing and we're still paused, queue another turn.
    if result.is_ok()
        && !shared.player.borrow().state().playing
        && !shared.restarting.get()
        && shared.scrub_epoch.get() == scrub_epoch
    {
        let playhead = shared.player.borrow().state().frame;
        let index = shared.player.borrow().track().index().clone();
        if let Some(playhead_key) = index.nearest_keyframe(playhead) {
            let prev_key = index.get(playhead_key).and_then(|e| {
                index
                    .frames()
                    .iter()
                    .filter(|x| x.is_sync && x.decode_order < e.decode_order)
                    .max_by_key(|x| x.decode_order)
                    .map(|x| x.frame)
            });
            let next_key = index.next_keyframe_after(playhead_key);
            let behind_first = shared.fill_bias.get() == FillBias::Behind;
            let desired = DenseFrameCache::desired_chunk_keys(
                playhead_key,
                prev_key,
                next_key,
                behind_first,
            );
            if !shared.dense_cache.borrow().missing_among(&desired).is_empty()
                && !shared.scrub_busy.get()
            {
                shared.scrub_busy.set(true);
                let again = Rc::clone(&shared);
                wasm_bindgen_futures::spawn_local(async move {
                    yield_event_loop().await;
                    let _ = ensure_window_task(again).await;
                });
            }
        }
    }
    result
}

async fn fill_one_chunk(shared: Rc<Shared>, key: FrameId) -> Result<(), MediaError> {
    let scrub_epoch = shared.scrub_epoch.get();
    let cancelled = || {
        shared.scrub_epoch.get() != scrub_epoch || shared.player.borrow().state().playing
    };
    if cancelled() {
        return Err(MediaError::Cancelled);
    }
    if shared.dense_cache.borrow().missing_among(&[key]).is_empty() {
        return Ok(());
    }

    let meta = shared.player.borrow().metadata().clone();
    let track = shared.player.borrow().track_rc();

    let mut decoder = match shared.scrub_decoder.borrow_mut().take() {
        Some(d) => d,
        None => WebCodecsDecoder::create(&meta).await?,
    };
    if cancelled() {
        *shared.scrub_decoder.borrow_mut() = Some(decoder);
        return Err(MediaError::Cancelled);
    }

    let decoded = decoder.decode_gop(track.as_ref(), key, &cancelled).await;
    match decoded {
        Ok(frames) => {
            shared.dense_cache.borrow_mut().insert_gop(key, frames);
            *shared.scrub_decoder.borrow_mut() = Some(decoder);
            Ok(())
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

fn performance_now_ms() -> Option<f64> {
    web_sys::window()?.performance().map(|p| p.now())
}

/// One event-loop turn via `Promise.resolve` (microtask).
async fn yield_event_loop() {
    let _ = wasm_bindgen_futures::JsFuture::from(js_sys::Promise::resolve(
        &wasm_bindgen::JsValue::UNDEFINED,
    ))
    .await;
}

async fn sleep_ms(ms: i32) {
    let promise = js_sys::Promise::new(&mut |resolve, _reject| {
        let _ = web_sys::window()
            .expect("window")
            .set_timeout_with_callback_and_timeout_and_arguments_0(&resolve, ms);
    });
    let _ = wasm_bindgen_futures::JsFuture::from(promise).await;
}
