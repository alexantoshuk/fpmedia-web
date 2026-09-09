//! High-level browser player: demux + WebCodecs + canvas.
//!
//! Present and decode share the player via interior mutability so an rAF
//! present loop never waits on `prefetch_one` (mediabunny-style split).

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::rc::Rc;

use web_sys::{CanvasRenderingContext2d, HtmlCanvasElement, VideoFrame};

use crate::decode::canvas::{canvas_2d, configure_canvas_for_video, draw_frame_with_context};
use crate::decode::webcodecs::WebCodecsDecoder;
use crate::error::MediaError;
use crate::player::{Player, PlayerState};
use crate::types::{FrameId, VideoMetadata};

/// Decoded frames held ready for canvas present (mediabunny-style prefetch).
pub const PRESENTATION_QUEUE_CAP: usize = 16;
/// Frames (incl. frame 0) decoded on a side decoder before the loop seam.
/// Prepared in parallel with end-of-clip playback so `loop_to_start` is a swap.
const LOOP_PRIME_FRAMES: usize = 8;
/// Wall-clock lag above this → skip to nearest keyframe instead of exact frame.
const BIG_LAG_SECONDS: f64 = 0.5;
/// Stop prefetching this many ms before the present deadline.
const PREFETCH_DEADLINE_SLACK_MS: f64 = 3.0;

struct QueuedFrame {
    frame: FrameId,
    video: VideoFrame,
}

struct Shared {
    player: RefCell<Player>,
    presentation: RefCell<VecDeque<QueuedFrame>>,
    /// Start-of-clip frames ready for the next loop wrap (side decoder).
    loop_prime: RefCell<Option<VecDeque<QueuedFrame>>>,
    canvas: HtmlCanvasElement,
    ctx: CanvasRenderingContext2d,
    frames_skipped: Cell<u64>,
    last_behind_ms: Cell<Option<f64>>,
    /// Prefetch must idle while seek/loop restart owns the main decoder.
    restarting: Cell<bool>,
    /// Side-decoder loop prepare in flight.
    preparing_loop: Cell<bool>,
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
    /// Optional second decoder used to prime the next loop wrap off the hot path.
    loop_decoder: RefCell<Option<WebCodecsDecoder>>,
}

impl WebPlayer {
    pub async fn open_mp4_bytes(
        bytes: Vec<u8>,
        canvas: HtmlCanvasElement,
    ) -> Result<Self, MediaError> {
        let player = Player::open_mp4_bytes(bytes)?;
        let meta = player.metadata().clone();
        configure_canvas_for_video(&canvas, meta.width, meta.height)?;
        let decoder = WebCodecsDecoder::create(&meta).await?;
        let ctx = canvas_2d(&canvas)?;
        let shared = Rc::new(Shared {
            player: RefCell::new(player),
            presentation: RefCell::new(VecDeque::new()),
            loop_prime: RefCell::new(None),
            canvas,
            ctx,
            frames_skipped: Cell::new(0),
            last_behind_ms: Cell::new(None),
            restarting: Cell::new(false),
            preparing_loop: Cell::new(false),
        });
        // Do not seek here — first-frame decode can be slow; let the host show
        // metadata first and call `seek_frame(0)` with its own progress UI.
        Ok(Self {
            shared,
            decoder: RefCell::new(Some(decoder)),
            loop_decoder: RefCell::new(None),
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
        self.shared.player.borrow_mut().play();
        self.shared.frames_skipped.set(0);
        self.shared.last_behind_ms.set(None);
    }

    pub fn pause(&self) {
        self.shared.player.borrow_mut().pause();
        self.clear_loop_prime();
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
        self.shared.restarting.set(true);
        let result = async {
            self.shared.player.borrow_mut().pause();
            self.clear_loop_prime();
            self.clear_presentation();
            self.invalidate_decoder();
            self.goto_frame(frame).await
        }
        .await;
        self.shared.restarting.set(false);
        result
    }

    pub async fn next_frame(&self) -> Result<(), MediaError> {
        self.shared.player.borrow_mut().pause();
        self.clear_presentation();
        let next = self.state().frame.saturating_add(1);
        self.goto_frame(next).await
    }

    pub async fn previous_frame(&self) -> Result<(), MediaError> {
        self.shared.player.borrow_mut().pause();
        self.clear_presentation();
        let cur = self.state().frame;
        if cur == 0 {
            return Err(MediaError::FrameOutOfRange(0, self.state().frame_count));
        }
        self.invalidate_decoder();
        self.goto_frame(cur - 1).await
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
                self.present(queued.video)?;
                self.shared.player.borrow_mut().commit_frame(queued.frame)?;
                return Ok(self.has_more_playback());
            }
        }

        self.clear_presentation();
        if behind_seconds >= BIG_LAG_SECONDS {
            self.invalidate_decoder();
        }
        self.goto_frame(target).await?;
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
        self.goto_frame(0).await?;
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
        self.fill_presentation_queue().await
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
            self.present(queued.video)?;
            self.shared.player.borrow_mut().commit_frame(queued.frame)?;
            did_draw = true;
            break;
        }

        Ok(DuePresent::Playing { did_draw })
    }

    pub async fn loop_to_start(&self) -> Result<(), MediaError> {
        // Finish an in-flight parallel prepare rather than starting a cold seek.
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

    /// Decode the next loop wrap on a side decoder while the main path still
    /// plays the end of the clip. No-op if not looping, already primed, or busy.
    pub async fn prepare_loop_if_needed(&self) -> Result<(), MediaError> {
        if !self.is_looping() || !self.is_playing() {
            return Ok(());
        }
        if self.shared.loop_prime.borrow().is_some() || self.shared.preparing_loop.get() {
            return Ok(());
        }
        // Wait until the main queue has content so we don't contend with Play prime.
        if self.presentation_len() == 0 {
            return Ok(());
        }

        self.shared.preparing_loop.set(true);
        let result = self.fill_loop_prime_parallel().await;
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
        self.present(first.video)?;
        self.shared.player.borrow_mut().commit_frame(first.frame)?;
        *self.shared.presentation.borrow_mut() = buf;
        self.swap_in_loop_decoder();
        Ok(true)
    }

    fn swap_in_loop_decoder(&self) {
        let primed = self.loop_decoder.borrow_mut().take();
        let main = self.decoder.borrow_mut().take();
        match (primed, main) {
            (Some(primed), Some(mut old)) => {
                *self.decoder.borrow_mut() = Some(primed);
                old.invalidate();
                *self.loop_decoder.borrow_mut() = Some(old);
            }
            (Some(primed), None) => {
                *self.decoder.borrow_mut() = Some(primed);
            }
            (None, Some(main)) => {
                *self.decoder.borrow_mut() = Some(main);
            }
            (None, None) => {}
        }
    }

    async fn fill_loop_prime_parallel(&self) -> Result<(), MediaError> {
        let meta = self.metadata();
        let mut dec = match self.loop_decoder.borrow_mut().take() {
            Some(mut d) => {
                d.invalidate();
                d
            }
            None => WebCodecsDecoder::create(&meta).await?,
        };

        let track = self.shared.player.borrow().track_rc();
        let count = self.state().frame_count;
        let n = (LOOP_PRIME_FRAMES as FrameId).clamp(1, count.max(1));
        let mut buf: VecDeque<QueuedFrame> = VecDeque::new();

        for frame in 0..n {
            if !self.is_playing() || !self.is_looping() {
                for q in buf.drain(..) {
                    q.video.close();
                }
                *self.loop_decoder.borrow_mut() = Some(dec);
                return Ok(());
            }
            match dec.decode_frame(track.as_ref(), frame).await {
                Ok(video) => buf.push_back(QueuedFrame { frame, video }),
                Err(e) => {
                    for q in buf.drain(..) {
                        q.video.close();
                    }
                    *self.loop_decoder.borrow_mut() = Some(dec);
                    return Err(e);
                }
            }
        }

        *self.loop_decoder.borrow_mut() = Some(dec);
        *self.shared.loop_prime.borrow_mut() = Some(buf);
        Ok(())
    }

    async fn loop_to_start_cold(&self) -> Result<(), MediaError> {
        self.shared.restarting.set(true);
        let result = async {
            self.clear_loop_prime();
            self.clear_presentation();
            self.invalidate_decoder();
            self.goto_frame(0).await?;
            if self.is_playing() {
                self.prime_presentation(LOOP_PRIME_FRAMES.saturating_sub(1))
                    .await?;
            }
            Ok(())
        }
        .await;
        self.shared.restarting.set(false);
        result
    }

    /// Decode one frame into the queue. May run concurrently with [`Self::present_due`].
    ///
    /// No-ops while [`Self::is_restarting`] — the restart/seek path owns the decoder and
    /// fills via [`Self::prime_presentation`] (which bypasses this gate).
    pub async fn prefetch_one(&self) -> Result<bool, MediaError> {
        if !self.is_playing() || self.shared.restarting.get() {
            return Ok(false);
        }
        self.enqueue_next_presentation_frame().await
    }

    /// Push the next presentation-order frame (ignores the restarting gate).
    async fn enqueue_next_presentation_frame(&self) -> Result<bool, MediaError> {
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
        let video = self.decode_frame(track.as_ref(), next).await?;
        if !self.is_playing() {
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
                    self.present(queued.video)?;
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
                        self.present(queued.video)?;
                        self.shared.player.borrow_mut().commit_frame(queued.frame)?;
                        return Ok(());
                    }
                } else {
                    drop(presentation);
                    self.clear_presentation();
                }
            }
        }
        self.goto_frame(frame).await
    }

    async fn fill_presentation_queue(&self) -> Result<(), MediaError> {
        self.prime_presentation(PRESENTATION_QUEUE_CAP).await
    }

    /// Decode up to `min_frames` into the presentation queue (bypasses restarting gate).
    async fn prime_presentation(&self, min_frames: usize) -> Result<(), MediaError> {
        let target = min_frames.min(PRESENTATION_QUEUE_CAP);
        while self.presentation_len() < target {
            if !self.enqueue_next_presentation_frame().await? {
                break;
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

    fn clear_loop_prime(&self) {
        if let Some(buf) = self.shared.loop_prime.borrow_mut().take() {
            for q in buf {
                q.video.close();
            }
        }
    }

    fn invalidate_decoder(&self) {
        if let Some(decoder) = self.decoder.borrow_mut().as_mut() {
            decoder.invalidate();
        }
    }

    /// Take the decoder out of the slot for the duration of an async decode so
    /// we never hold `RefMut` across `.await` (avoids freezes / panics).
    async fn decode_frame(
        &self,
        track: &crate::demux::Mp4VideoTrack,
        frame: FrameId,
    ) -> Result<VideoFrame, MediaError> {
        let mut decoder = self
            .decoder
            .borrow_mut()
            .take()
            .ok_or_else(|| MediaError::WebCodecs("decoder busy".into()))?;
        let result = decoder.decode_frame(track, frame).await;
        *self.decoder.borrow_mut() = Some(decoder);
        result
    }

    async fn goto_frame(&self, frame: FrameId) -> Result<(), MediaError> {
        let track = self.shared.player.borrow().track_rc();
        let video_frame = self.decode_frame(track.as_ref(), frame).await?;
        self.present(video_frame)?;
        self.shared.player.borrow_mut().commit_frame(frame)?;
        Ok(())
    }

    fn present(&self, frame: VideoFrame) -> Result<(), MediaError> {
        let result =
            draw_frame_with_context(&self.shared.canvas, &self.shared.ctx, &frame);
        frame.close();
        result
    }
}

impl Drop for WebPlayer {
    fn drop(&mut self) {
        self.clear_loop_prime();
        self.clear_presentation();
    }
}

fn performance_now_ms() -> Option<f64> {
    web_sys::window()?.performance().map(|p| p.now())
}

async fn sleep_ms(ms: i32) {
    let promise = js_sys::Promise::new(&mut |resolve, _reject| {
        let _ = web_sys::window()
            .expect("window")
            .set_timeout_with_callback_and_timeout_and_arguments_0(&resolve, ms);
    });
    let _ = wasm_bindgen_futures::JsFuture::from(promise).await;
}
