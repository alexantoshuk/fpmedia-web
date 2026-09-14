//! High-level browser player: demux + WebCodecs + canvas.
//!
//! Present and decode share the player via interior mutability so an rAF
//! present loop never waits on `prefetch_one` (mediabunny-style split).

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::rc::Rc;

use web_sys::{CanvasRenderingContext2d, HtmlCanvasElement};

use crate::decode::owned_video_frame::OwnedVideoFrame;

use crate::decode::canvas::{canvas_2d, configure_canvas_for_video, draw_frame_with_context};
use crate::decode::frame_cache::{
    DenseFrameCache, ThumbCache, DEFAULT_MAX_THUMBS, DEFAULT_MAX_THUMB_GOPS,
};
use crate::decode::webcodecs::WebCodecsDecoder;
use crate::error::MediaError;
use crate::player::{Player, PlayerState};
use crate::types::{FrameId, GopProfile, VideoMetadata};

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
    video: OwnedVideoFrame,
}

/// Single playhead cache, two fill policies (presentation queue still delivers play).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CacheFillMode {
    /// Ahead via presentation queue; gap-fill cancelled.
    Play,
    /// Paused/scrub: fill visited gaps both sides of the playhead.
    Scrub,
}

struct Shared {
    player: RefCell<Player>,
    presentation: RefCell<VecDeque<QueuedFrame>>,
    /// Unified playhead cache (visited + gap-filled frames).
    dense_cache: RefCell<DenseFrameCache>,
    /// Filmstrip thumbs (caller-sized); separate budget from dense.
    thumb_cache: RefCell<ThumbCache>,
    /// Second decoder for background gap fill (does not steal main playback).
    scrub_decoder: RefCell<Option<WebCodecsDecoder>>,
    scrub_epoch: Cell<u64>,
    scrub_busy: Cell<bool>,
    /// Set while a fill is running so `schedule_scrub_cache_fill` can request another
    /// pass (avoids dropping work when pause/scrub bumps epoch mid-fill).
    scrub_pending: Cell<bool>,
    fill_mode: Cell<CacheFillMode>,
    /// Start frames decoded ahead of the loop seam (same decoder, while end plays).
    loop_prime: RefCell<Option<VecDeque<QueuedFrame>>>,
    canvas: HtmlCanvasElement,
    ctx: CanvasRenderingContext2d,
    frames_skipped: Cell<u64>,
    last_behind_ms: Cell<Option<f64>>,
    /// Prefetch must idle while seek/loop prepare owns the decoder.
    restarting: Cell<bool>,
    /// Settle / sync seek: pause densify so exact `goto_frame` owns HW.
    /// Drag scrub leaves this false so gap-fill can warm the cache under the finger.
    hold_densify_for_exact: Cell<bool>,
    preparing_loop: Cell<bool>,
    /// Bumped to cancel in-flight decode (mediabunny dispose / asyncId).
    op_epoch: Cell<u64>,
    /// Set when invalidate runs while the decoder slot is empty (checked out).
    /// Honored on the next `take_decoder` put-back / decode start.
    decoder_reset: Cell<bool>,
    /// After fill stalls (no progress), skip restart until aim leaves this frame.
    fill_stalled_aim: Cell<Option<FrameId>>,
}

/// Why a frame is marked ready on the demo timeline strip.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReadyFrameKind {
    /// Exact decoded frame in the playhead cache.
    Exact,
    /// In the play presentation / ahead queue.
    Ahead,
}

/// One ready frame for UI timeline marks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReadyFrameMark {
    pub frame: FrameId,
    pub kind: ReadyFrameKind,
}

/// Live decode / prefetch counters for the demo UI (and host apps).
#[derive(Clone, Debug, PartialEq)]
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
    pub cache_cap: usize,
    /// Dense extent is a single contiguous run (gap-fill edge-armed only).
    pub cache_no_gaps: bool,
    pub scrub_busy: bool,
    /// Inclusive cache extent `[from, to]` (presentation frames), if any.
    pub scrub_window_from: Option<FrameId>,
    pub scrub_window_to: Option<FrameId>,
    /// Frames ready in cache / presentation queue (timeline strip). Sorted by frame.
    pub ready_frames: Vec<ReadyFrameMark>,
    /// Keyframe spacing class (scrub budget / fill policy).
    pub gop_profile: GopProfile,
}

impl Default for PipelineStats {
    fn default() -> Self {
        Self {
            presentation_len: 0,
            presentation_cap: PRESENTATION_QUEUE_CAP,
            presentation_from: None,
            presentation_to: None,
            held_len: 0,
            decode_queue_size: 0,
            frames_skipped: 0,
            last_behind_ms: None,
            cache_chunks: 0,
            cache_frames: 0,
            cache_cap: 0,
            cache_no_gaps: false,
            scrub_busy: false,
            scrub_window_from: None,
            scrub_window_to: None,
            ready_frames: Vec::new(),
            gop_profile: GopProfile::Long { median: 0, max: 0 },
        }
    }
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
        let max_frames = DenseFrameCache::budget_for_clip(&meta);
        let shared = Rc::new(Shared {
            player: RefCell::new(player),
            presentation: RefCell::new(VecDeque::new()),
            dense_cache: RefCell::new(DenseFrameCache::new(max_frames)),
            thumb_cache: RefCell::new(ThumbCache::new(
                DEFAULT_MAX_THUMB_GOPS,
                DEFAULT_MAX_THUMBS,
            )),
            scrub_decoder: RefCell::new(None),
            scrub_epoch: Cell::new(0),
            scrub_busy: Cell::new(false),
            scrub_pending: Cell::new(false),
            fill_mode: Cell::new(CacheFillMode::Scrub),
            loop_prime: RefCell::new(None),
            canvas,
            ctx,
            frames_skipped: Cell::new(0),
            last_behind_ms: Cell::new(None),
            restarting: Cell::new(false),
            hold_densify_for_exact: Cell::new(false),
            preparing_loop: Cell::new(false),
            op_epoch: Cell::new(0),
            decoder_reset: Cell::new(false),
            fill_stalled_aim: Cell::new(None),
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
        self.shared.fill_mode.set(CacheFillMode::Play);
        // WASM is single-threaded — cancel scrub cache fill so it cannot
        // steal the event loop from present/prefetch.
        self.cancel_ops_and_scrub_fill();
        self.shared.player.borrow_mut().play();
        self.shared.frames_skipped.set(0);
        self.shared.last_behind_ms.set(None);
    }

    pub fn pause(&self) {
        // Leaving play: cancel decode + scrub fill epoch (pending latch restarts fill).
        // Already paused: do not bump scrub_epoch (UI steps call pause every time).
        let was_playing = self.shared.player.borrow().state().playing;
        if was_playing {
            self.cancel_ops_and_scrub_fill();
        }
        self.shared.player.borrow_mut().pause();
        self.clear_loop_prime();
        // Same pipeline as scrub drag / hold / release.
        self.enter_scrub_cache_pipeline(self.state().frame);
    }

    /// Aim + park only — no densify kick. Used while the main decoder owns an
    /// exact seek/scrub (`restarting`) so the scrub decoder does not compete for
    /// the HW session until the finger frame is on canvas.
    fn aim_scrub_cache(&self, focus: FrameId) {
        self.shared.fill_mode.set(CacheFillMode::Scrub);
        self.park_presentation_into_cache();
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
    ///    (skipped for [`GopProfile::AllIntra`] — single-frame seek is cheap;
    ///    visited frames still land in dense via `present_and_cache`)
    ///
    /// While [`Shared::hold_densify_for_exact`] (scrub settle / sync seek), densify
    /// is deferred so exact `goto_frame` owns HW; drag scrub keeps densify on.
    pub fn enter_scrub_cache_pipeline(&self, focus: FrameId) {
        self.aim_scrub_cache(focus);
        if !self.is_all_intra() {
            self.schedule_scrub_cache_fill();
        }
    }

    fn is_all_intra(&self) -> bool {
        matches!(
            self.shared.player.borrow().metadata().gop_profile,
            GopProfile::AllIntra
        )
    }

    /// Bump the op epoch so in-flight decode/seek ignores its result.
    /// Does **not** cancel background scrub fill — scrub/seek call [`Self::begin_op`]
    /// often; killing fill on every pointermove left the cache stuck on the old GOP.
    pub fn cancel_ops(&self) {
        let next = self.shared.op_epoch.get().wrapping_add(1);
        self.shared.op_epoch.set(next);
    }

    /// Cancel decode **and** background dense fill (play / hard abort).
    pub fn cancel_ops_and_scrub_fill(&self) {
        self.cancel_ops();
        self.abort_scrub_fill(true);
    }

    /// Stop densify immediately and free the scrub `VideoDecoder` HW session.
    ///
    /// Long-GOP settle must not compete with exact cascade on the same GPU
    /// (mediabunny: seekToKey/seekTo own the decoder; densify waits).
    /// `rearm` → latch `scrub_pending` so fill resumes after exact clears hold.
    fn abort_scrub_fill(&self, rearm: bool) {
        let scrub = self.shared.scrub_epoch.get().wrapping_add(1);
        self.shared.scrub_epoch.set(scrub);
        self.shared.scrub_pending.set(rearm);
        if let Some(mut d) = self.shared.scrub_decoder.borrow_mut().take() {
            let _ = d.reset();
        }
    }

    /// Pointer-up settle entry from the UI host — before the async op runs.
    ///
    /// Must not call [`Self::note_scrub_frame`] / densify kick: that starved exact
    /// on Wing It–class long GOPs (playhead far, `eng` stuck at 0, `decodeQ≈10`).
    pub fn prepare_scrub_settle(&self, frame: FrameId) {
        self.abort_scrub_fill(true);
        self.shared.hold_densify_for_exact.set(true);
        self.cancel_ops();
        // Drop in-flight main-decoder cascade from the preceding drag sample so
        // settle does not inherit a wedged `decodeQueueSize`.
        self.invalidate_decoder();
        self.aim_scrub_cache(frame);
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
        // Do not hold `presentation` across the dense-cache borrow — park/fill
        // may re-enter on VideoFrame drop and need the queue RefCell.
        let (presentation_len, presentation_from, presentation_to, ahead) = {
            let presentation = self.shared.presentation.borrow();
            (
                presentation.len(),
                presentation.front().map(|q| q.frame),
                presentation.back().map(|q| q.frame),
                presentation
                    .iter()
                    .map(|q| q.frame)
                    .collect::<Vec<_>>(),
            )
        };
        let (cache_chunks, cache_frames, cache_cap, cache_no_gaps, ready_frames, win_from, win_to) = {
            let cache = self.shared.dense_cache.borrow();
            let extent = cache.extent();
            let mut ready = Vec::new();
            for id in cache.cached_frame_ids() {
                ready.push(ReadyFrameMark {
                    frame: id,
                    kind: ReadyFrameKind::Exact,
                });
            }
            for frame in ahead {
                if !ready.iter().any(|m| m.frame == frame) {
                    ready.push(ReadyFrameMark {
                        frame,
                        kind: ReadyFrameKind::Ahead,
                    });
                } else if let Some(m) = ready.iter_mut().find(|m| m.frame == frame) {
                    m.kind = ReadyFrameKind::Ahead;
                }
            }
            ready.sort_by_key(|m| m.frame);
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
            presentation_cap: PRESENTATION_QUEUE_CAP,
            presentation_from,
            presentation_to,
            held_len,
            decode_queue_size,
            frames_skipped: self.shared.frames_skipped.get(),
            last_behind_ms: self.shared.last_behind_ms.get(),
            cache_chunks,
            cache_frames,
            cache_cap,
            cache_no_gaps,
            scrub_busy: self.shared.scrub_busy.get(),
            scrub_window_from: win_from,
            scrub_window_to: win_to,
            ready_frames,
            gop_profile: self.shared.player.borrow().metadata().gop_profile,
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
        // Exact first — abort densify and hold so scrub HW cannot starve the cascade.
        self.abort_scrub_fill(true);
        self.shared.hold_densify_for_exact.set(true);
        self.shared.restarting.set(true);
        let result = async {
            self.shared.player.borrow_mut().pause();
            self.clear_loop_prime();
            self.aim_scrub_cache(frame);
            if self.present_from_cache(frame)? {
                self.shared.player.borrow_mut().commit_frame(frame)?;
                return Ok(());
            }
            self.goto_frame(frame, epoch).await
        }
        .await;
        self.shared.restarting.set(false);
        self.shared.hold_densify_for_exact.set(false);
        if !self.is_current(epoch) {
            return Err(MediaError::Cancelled);
        }
        result?;
        self.enter_scrub_cache_pipeline(self.state().frame);
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

    /// Scrub pointer sample (drag / hold). Same cache pipeline as pause + release.
    pub fn note_scrub_frame(&self, frame: FrameId) {
        self.enter_scrub_cache_pipeline(frame);
    }

    /// Async timeline scrub (mediabunny `seekToKey` then `seekTo` under one epoch).
    ///
    /// - **Drag** (`settle=false`): densify while moving (warm cache under the finger);
    ///   preview + best-effort exact may race the scrub decoder.
    /// - **Settle** (`settle=true`, pointer up): abort densify, seekToKey then warm
    ///   seekTo (`goto_frame`) so exact owns HW — not while scrub densify holds
    ///   `decodeQ`, and not via a `fill_gop` key re-feed that wedges long GOPs.
    pub async fn scrub_to(&self, frame: FrameId, settle: bool) -> Result<(), MediaError> {
        let epoch = self.begin_op();
        if settle {
            // Host may already have called [`Self::prepare_scrub_settle`]; reinforce.
            self.abort_scrub_fill(true);
            self.shared.hold_densify_for_exact.set(true);
            self.shared.restarting.set(true);
            self.aim_scrub_cache(frame);
        } else {
            self.shared.hold_densify_for_exact.set(false);
            self.shared.restarting.set(true);
            // Drag: kick densify immediately so the strip fills while the finger moves.
            self.enter_scrub_cache_pipeline(frame);
        }
        let result = async {
            self.shared.player.borrow_mut().pause();
            self.clear_loop_prime();

            if self.present_from_cache(frame)? {
                self.shared.player.borrow_mut().commit_frame(frame)?;
                return Ok(());
            }

            // Canvas-only key/neighbor preview — do not commit_frame (UI playhead stays).
            if let Some(preview) = self.scrub_preview_frame(frame) {
                if preview != frame {
                    self.draw_frame_unccommitted(preview, epoch).await?;
                    if !self.is_current(epoch) {
                        return Err(MediaError::Cancelled);
                    }
                    if self.present_from_cache(frame)? {
                        self.shared.player.borrow_mut().commit_frame(frame)?;
                        return Ok(());
                    }
                }
            }

            // Settle: mediabunny seekToKey → seekTo.
            // Commit the key first (leaves frame 0), then continue the warm cascade
            // to exact — do **not** `fill_gop` restart (re-feed of the same key after
            // `goto_frame(key)` wedged at decodeQ≈11 on Wing It long GOPs).
            if settle {
                if let Some(key) = self.key_for(frame) {
                    if key != frame {
                        if self.present_from_cache(key)? {
                            self.shared.player.borrow_mut().commit_frame(key)?;
                        } else {
                            self.goto_frame(key, epoch).await?;
                        }
                        if !self.is_current(epoch) {
                            return Err(MediaError::Cancelled);
                        }
                        if self.present_from_cache(frame)? {
                            self.shared.player.borrow_mut().commit_frame(frame)?;
                            return Ok(());
                        }
                    }
                }
            }

            self.goto_frame(frame, epoch).await
        }
        .await;
        self.shared.restarting.set(false);
        self.shared.hold_densify_for_exact.set(false);
        // Settle / drag end: densify at the final focus (also re-arms after hold).
        self.enter_scrub_cache_pipeline(frame);
        if !self.is_current(epoch) {
            return Err(MediaError::Cancelled);
        }
        result?;
        Ok(())
    }

    /// Best interim canvas frame while exact `target` is missing from dense cache.
    ///
    /// Prefer a cached neighbor when `|cached − target| ≤ |key − target|` (tie →
    /// cache: already decoded, no key jump). Otherwise the nearest sync key.
    fn scrub_preview_frame(&self, target: FrameId) -> Option<FrameId> {
        let key = self.key_for(target);
        let cached = self.shared.dense_cache.borrow().nearest_frame(target);
        crate::scrub_preview_choice(target, key, cached)
    }

    /// Decode/draw `frame` for canvas preview without updating the committed playhead.
    ///
    /// Cache hits draw in place. Key (uncached) previews decode without inserting
    /// into dense — orphan syncs must not punch holes outside the scrub window.
    async fn draw_frame_unccommitted(
        &self,
        frame: FrameId,
        epoch: u64,
    ) -> Result<(), MediaError> {
        if self.present_from_cache(frame)? {
            return Ok(());
        }
        let track = self.shared.player.borrow().track_rc();
        let video_frame = self.decode_frame(track.as_ref(), frame, epoch).await?;
        if !self.is_current(epoch) {
            drop(video_frame);
            return Err(MediaError::Cancelled);
        }
        self.present_only(video_frame)
    }

    pub async fn next_frame(&self) -> Result<(), MediaError> {
        self.shared.player.borrow_mut().pause();
        let next = self.state().frame.saturating_add(1);
        // Reuse prefetch when present — clearing the queue would close the only
        // copy of frames the warm decoder already emitted.
        {
            let mut presentation = self.shared.presentation.borrow_mut();
            if presentation.front().is_some_and(|f| f.frame == next) {
                let queued = presentation.pop_front().expect("front checked");
                drop(presentation);
                self.present_and_cache(queued.frame, queued.video)?;
                self.shared.player.borrow_mut().commit_frame(queued.frame)?;
                self.enter_scrub_cache_pipeline(queued.frame);
                return Ok(());
            }
        }
        // Instant next from dense cache (e.g. after stepping back).
        if self.present_from_cache(next)? {
            self.shared.player.borrow_mut().commit_frame(next)?;
            self.enter_scrub_cache_pipeline(next);
            return Ok(());
        }
        let epoch = self.begin_op();
        self.enter_scrub_cache_pipeline(next);
        self.goto_frame(next, epoch).await?;
        self.enter_scrub_cache_pipeline(self.state().frame);
        Ok(())
    }

    pub async fn previous_frame(&self) -> Result<(), MediaError> {
        self.shared.player.borrow_mut().pause();
        let cur = self.state().frame;
        if cur == 0 {
            return Err(MediaError::FrameOutOfRange(0, self.state().frame_count));
        }
        let prev = cur - 1;
        // Prefer dense cache — key→N cascade is multi-second on long GOPs.
        if self.present_from_cache(prev)? {
            self.shared.player.borrow_mut().commit_frame(prev)?;
            self.enter_scrub_cache_pipeline(prev);
            return Ok(());
        }
        let epoch = self.begin_op();
        self.enter_scrub_cache_pipeline(prev);
        // Cache miss: densify the GOP (keep intermediates for the next PREV).
        // Present `prev` as soon as it emerges mid-GOP — do not wait for the tail.
        if let Some(key) = self.key_for(prev) {
            match self.fill_gop_into_cache(key, epoch, Some(prev)).await {
                Ok(()) => {
                    if !self.is_current(epoch) {
                        return Err(MediaError::Cancelled);
                    }
                    if self.state().frame == prev || self.present_from_cache(prev)? {
                        if self.state().frame != prev {
                            self.shared.player.borrow_mut().commit_frame(prev)?;
                        }
                        self.enter_scrub_cache_pipeline(prev);
                        return Ok(());
                    }
                }
                Err(MediaError::Cancelled) => return Err(MediaError::Cancelled),
                Err(_) => {
                    // Fall through to single-frame cascade.
                }
            }
        }
        self.invalidate_decoder();
        self.goto_frame(prev, epoch).await?;
        self.enter_scrub_cache_pipeline(self.state().frame);
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
                // Visited play frames belong in dense (same as goto / step).
                self.present_and_cache(queued.frame, queued.video)?;
                self.touch_playhead(queued.frame);
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
                drop(queued.video);
                self.shared.player.borrow_mut().commit_frame(queued.frame)?;
                continue;
            }

            let lag_ms = ((media_time_secs - start) * 1000.0).max(0.0);
            if lag_ms > 1.0 {
                self.shared.last_behind_ms.set(Some(lag_ms));
            }
            // Play presents are visited frames — park in dense so pause gap-fill
            // starts from a playhead-centered window (not a stale open/seek island).
            self.present_and_cache(queued.frame, queued.video)?;
            self.touch_playhead(queued.frame);
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
        self.present_and_cache(first.frame, first.video)?;
        self.touch_playhead(first.frame);
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
                    drop(q.video);
                }
                return Ok(());
            }
            match self.decode_frame(track.as_ref(), frame, epoch).await {
                Ok(video) => buf.push_back(QueuedFrame { frame, video }),
                Err(MediaError::Cancelled) => {
                    for q in buf.drain(..) {
                        drop(q.video);
                    }
                    return Ok(());
                }
                Err(e) => {
                    for q in buf.drain(..) {
                        drop(q.video);
                    }
                    return Err(e);
                }
            }
        }

        if !self.is_current(epoch) {
            for q in buf.drain(..) {
                drop(q.video);
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
            drop(video);
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
                    self.present_and_cache(queued.frame, queued.video)?;
                    self.touch_playhead(queued.frame);
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
                        self.present_and_cache(queued.frame, queued.video)?;
                        self.touch_playhead(queued.frame);
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
                drop(q.video);
            }
        }
    }

    fn clear_presentation(&self) {
        let mut presentation = self.shared.presentation.borrow_mut();
        while let Some(q) = presentation.pop_front() {
            drop(q.video);
        }
    }

    /// Move presentation-queue frames into the dense cache (pause / scrub handoff).
    ///
    /// Drain the queue first, then insert — do not hold `presentation` across
    /// `insert_frame` (evict/drop of `VideoFrame` can re-enter JS → Rust and
    /// panic with `RefCell already borrowed`).
    fn park_presentation_into_cache(&self) {
        let items: Vec<(FrameId, OwnedVideoFrame)> = {
            let mut presentation = self.shared.presentation.borrow_mut();
            presentation
                .drain(..)
                .map(|q| (q.frame, q.video))
                .collect()
        };
        for (frame, video) in items {
            let Ok(mut cache) = self.shared.dense_cache.try_borrow_mut() else {
                // Nested re-entry during another insert — drop rather than panic.
                drop(video);
                continue;
            };
            cache.insert_frame(frame, video);
        }
    }

    fn clear_dense_cache(&self) {
        self.shared.dense_cache.borrow_mut().clear();
        self.shared.thumb_cache.borrow_mut().clear();
        // Abort in-flight gap fill.
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
        let Some(key) = self.key_for(frame) else {
            return;
        };
        // `insert_frame` eviction drops `VideoFrame`s; Chromium may flush microtasks
        // mid-drop and re-enter scrub (`note_scrub` → here) while the cache RefCell
        // is still mutably borrowed — common when aim jumps to 0 and many far frames
        // evict. Skip the nested aim update; the outer insert still completes.
        let Ok(mut cache) = self.shared.dense_cache.try_borrow_mut() else {
            return;
        };
        cache.touch_playhead(frame, key);
    }

    fn clone_from_dense_cache(&self, frame: FrameId) -> Result<Option<OwnedVideoFrame>, MediaError> {
        let Ok(cache) = self.shared.dense_cache.try_borrow() else {
            return Ok(None);
        };
        cache.try_clone_frame(frame)
    }

    /// Draw a cached frame without cloning the `VideoFrame` (clone often fails).
    fn present_from_cache(&self, frame: FrameId) -> Result<bool, MediaError> {
        let Ok(cache) = self.shared.dense_cache.try_borrow() else {
            return Ok(false);
        };
        let drawn = cache.with_frame(frame, |video| {
            draw_frame_with_context(
                &self.shared.canvas,
                &self.shared.ctx,
                video.as_web_sys(),
            )
        });
        match drawn {
            Some(Ok(())) => Ok(true),
            Some(Err(e)) => Err(e),
            None => Ok(false),
        }
    }

    fn clear_loop_prime(&self) {
        if let Some(buf) = self.shared.loop_prime.borrow_mut().take() {
            for q in buf {
                drop(q.video);
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
    ) -> Result<OwnedVideoFrame, MediaError> {
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
                drop(video);
            }
            return Err(MediaError::Cancelled);
        }
        *self.decoder.borrow_mut() = Some(decoder);
        result
    }

    async fn take_decoder(&self, epoch: u64) -> Result<WebCodecsDecoder, MediaError> {
        // Macrotask yields — the peer that holds the decoder awaits `setTimeout(0)` /
        // WebCodecs output. Microtask-only spins (Promise.resolve) can burn the
        // whole wait in one turn and return "decoder busy" before the holder resumes.
        for _ in 0..120 {
            if !self.is_current(epoch) {
                return Err(MediaError::Cancelled);
            }
            if let Some(decoder) = self.decoder.borrow_mut().take() {
                return Ok(decoder);
            }
            sleep_ms(0).await;
        }
        Err(MediaError::WebCodecs(
            "decoder busy (Pause/Play raced — try Play again)".into(),
        ))
    }

    async fn goto_frame(&self, frame: FrameId, epoch: u64) -> Result<(), MediaError> {
        let track = self.shared.player.borrow().track_rc();
        let video_frame = self.decode_frame(track.as_ref(), frame, epoch).await?;
        if !self.is_current(epoch) {
            drop(video_frame);
            return Err(MediaError::Cancelled);
        }
        self.present_and_cache(frame, video_frame)?;
        self.shared.player.borrow_mut().commit_frame(frame)?;
        Ok(())
    }

    /// Draw and remember a visited frame in the dense cache.
    ///
    /// Inserts by **move** after draw — do not rely on `VideoFrame.clone()`
    /// (mediabunny keeps samples; browser clone often fails → empty cache).
    fn present_and_cache(&self, frame: FrameId, video: OwnedVideoFrame) -> Result<(), MediaError> {
        let result = draw_frame_with_context(
            &self.shared.canvas,
            &self.shared.ctx,
            video.as_web_sys(),
        );
        if let Ok(mut cache) = self.shared.dense_cache.try_borrow_mut() {
            cache.insert_frame(frame, video);
        } else {
            drop(video);
        }
        result
    }

    /// Draw without inserting (play path — presentation queue owns ahead frames).
    fn present_only(&self, video: OwnedVideoFrame) -> Result<(), MediaError> {
        let result = draw_frame_with_context(
            &self.shared.canvas,
            &self.shared.ctx,
            video.as_web_sys(),
        );
        drop(video);
        result
    }

    /// Exact frame from dense cache or scrub decode (clone / owned).
    pub async fn frame_at(&self, frame: FrameId) -> Result<OwnedVideoFrame, MediaError> {
        let count = self.state().frame_count;
        if frame >= count {
            return Err(MediaError::FrameOutOfRange(frame, count));
        }
        if let Some(video) = self.clone_from_dense_cache(frame)? {
            return Ok(video);
        }
        self.scrub_fill_frame(frame).await?;
        self.clone_from_dense_cache(frame)?.ok_or_else(|| {
            MediaError::WebCodecs(format!("frame_at miss after gap fill ({frame})"))
        })
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

    fn schedule_scrub_cache_fill(&self) {
        // Gate on play mode / All-Intra (single-packet seek — no densify).
        // Settle/seek: latch pending while exact owns HW (`hold_densify_for_exact`).
        if self.shared.fill_mode.get() == CacheFillMode::Play
            || self.is_playing()
            || self.is_all_intra()
        {
            return;
        }
        if self.shared.hold_densify_for_exact.get() || self.shared.scrub_busy.get() {
            self.shared.scrub_pending.set(true);
            return;
        }
        self.kick_scrub_cache_fill();
    }

    fn kick_scrub_cache_fill(&self) {
        if self.shared.fill_mode.get() == CacheFillMode::Play
            || self.is_playing()
            || self.is_all_intra()
            || self.shared.hold_densify_for_exact.get()
        {
            if self.shared.hold_densify_for_exact.get() {
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

    async fn scrub_fill_frame(&self, frame: FrameId) -> Result<(), MediaError> {
        let shared = Rc::clone(&self.shared);
        let epoch = shared.scrub_epoch.get();
        fill_one_gap(shared, frame, epoch).await.map(|_| ())
    }

    /// Decode a full GOP into the dense cache via the main decoder (PREV miss).
    /// Prefer this over `decode_frame` so intermediate frames are kept for the
    /// next PREV — and over scrub-cache fill so we do not race the fill loop.
    ///
    /// When `present_when` is set, paint + commit that frame as soon as it
    /// emerges (still finish the GOP for the cache).
    async fn fill_gop_into_cache(
        &self,
        key: FrameId,
        epoch: u64,
        present_when: Option<FrameId>,
    ) -> Result<(), MediaError> {
        {
            let index = self.shared.player.borrow().track().index().clone();
            let ids = index.gop_frame_ids(key).unwrap_or_default();
            if self.shared.dense_cache.borrow().gop_complete(key, &ids) {
                return Ok(());
            }
        }
        let track = self.shared.player.borrow().track_rc();
        let mut decoder = self.take_decoder(epoch).await?;
        if self.shared.decoder_reset.get() {
            decoder.reset()?;
            self.shared.decoder_reset.set(false);
        }
        let cancelled = || !self.is_current(epoch);
        let shared = Rc::clone(&self.shared);
        let presented = std::cell::Cell::new(false);
        let mut on_frame = move |id: FrameId, video: OwnedVideoFrame| {
            let Ok(mut cache) = shared.dense_cache.try_borrow_mut() else {
                drop(video);
                return;
            };
            cache.insert_frame(id, video);
            drop(cache);
            if present_when == Some(id) && !presented.get() {
                if present_cached_frame(&shared, id) {
                    if shared.player.borrow_mut().commit_frame(id).is_ok() {
                        presented.set(true);
                    }
                }
            }
        };
        let decoded = decoder
            .decode_gop(track.as_ref(), key, &cancelled, &mut on_frame)
            .await;
        match decoded {
            Ok(()) => {
                *self.decoder.borrow_mut() = Some(decoder);
                Ok(())
            }
            Err(MediaError::Cancelled) => {
                let _ = decoder.reset();
                *self.decoder.borrow_mut() = Some(decoder);
                Err(MediaError::Cancelled)
            }
            Err(e) => {
                let _ = decoder.reset();
                *self.decoder.borrow_mut() = Some(decoder);
                Err(e)
            }
        }
    }
}

impl Drop for WebPlayer {
    fn drop(&mut self) {
        self.clear_loop_prime();
        self.clear_presentation();
        self.clear_dense_cache();
    }
}

async fn scrub_cache_fill_task(shared: Rc<Shared>) -> Result<(), MediaError> {
    let mut scrub_epoch = shared.scrub_epoch.get();
    let result = async {
        let mut idle_passes = 0u8;
        loop {
            if shared.fill_mode.get() == CacheFillMode::Play
                || shared.player.borrow().state().playing
                || matches!(
                    shared.player.borrow().metadata().gop_profile,
                    GopProfile::AllIntra
                )
            {
                return Ok(());
            }
            // Settle / sync seek owns HW — exit so `scrub_busy` clears and the scrub
            // decoder is not pinned in a spin-wait (was starving exact on long GOPs).
            if shared.hold_densify_for_exact.get() {
                shared.scrub_pending.set(true);
                return Ok(());
            }
            // Pause/play bumped the fill epoch — adopt it and keep going if still scrubbing.
            if shared.scrub_epoch.get() != scrub_epoch {
                if shared.fill_mode.get() != CacheFillMode::Scrub
                    || shared.player.borrow().state().playing
                {
                    return Ok(());
                }
                scrub_epoch = shared.scrub_epoch.get();
                shared.scrub_pending.set(false);
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

            match fill_one_gap(Rc::clone(&shared), target, scrub_epoch).await {
                Ok(made_progress) => {
                    idle_passes = if made_progress { 0 } else { idle_passes.saturating_add(1) };
                    // No progress repeatedly (oversized GOP / budget) — stop spinning.
                    if idle_passes >= 3 {
                        shared.fill_stalled_aim.set(Some(playhead_and_last(&shared).0));
                        return Ok(());
                    }
                    // Macrotask so paused rAF can refresh the ready strip.
                    sleep_ms(0).await;
                }
                Err(MediaError::Cancelled) => {
                    // Re-aim, epoch bump, or hold-for-exact — continue while paused/scrub.
                    idle_passes = 0;
                    sleep_ms(0).await;
                }
                Err(_) => {
                    // Transient WebCodecs fault — keep densifying (do not kill the task).
                    idle_passes = idle_passes.saturating_add(1);
                    if idle_passes >= 3 {
                        shared.fill_stalled_aim.set(Some(playhead_and_last(&shared).0));
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
    // **not** respawn — a spawn_local spin froze the tab (Wing It cold scrub).
    if shared.hold_densify_for_exact.get() {
        return result;
    }

    let want_again = shared.scrub_pending.get()
        && shared.fill_mode.get() == CacheFillMode::Scrub
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

fn apply_clip_budgets(shared: &Shared) {
    let meta = shared.player.borrow().metadata().clone();
    let max_frames = DenseFrameCache::budget_for_clip(&meta);
    shared.dense_cache.borrow_mut().set_max_frames(max_frames);
}

/// Draw a dense-cache hit without cloning (insert already owns the frame).
fn present_cached_frame(shared: &Shared, frame: FrameId) -> bool {
    let Ok(cache) = shared.dense_cache.try_borrow() else {
        return false;
    };
    matches!(
        cache.with_frame(frame, |video| {
            draw_frame_with_context(&shared.canvas, &shared.ctx, video.as_web_sys())
        }),
        Some(Ok(()))
    )
}

fn playhead_and_last(shared: &Shared) -> (FrameId, FrameId) {
    let state = shared.player.borrow().state();
    let last = state.frame_count.saturating_sub(1);
    let committed = state.frame.min(last);
    let aimed = shared.dense_cache.borrow().playhead().min(last);
    // During scrub, aim follows the pointer; otherwise the committed playhead.
    let playhead = if aimed != committed { aimed } else { committed };
    (playhead, last)
}

/// Next missing frame nearest the playhead, or `None` when saturated /
/// disarmed / stalled at this aim.
fn next_gap_target(shared: &Shared) -> Option<FrameId> {
    let (playhead, last) = playhead_and_last(shared);
    if shared.fill_stalled_aim.get() == Some(playhead) {
        return None;
    }
    if shared.fill_stalled_aim.get().is_some() {
        // Aim moved off the stalled frame — fill may run again.
        shared.fill_stalled_aim.set(None);
    }
    let cache = shared.dense_cache.borrow();
    if cache.gaps_saturated(playhead, last) || !cache.gap_fill_armed(playhead) {
        return None;
    }
    cache.next_gap_frame(playhead, last)
}

/// Densify the GOP containing `target`. Returns whether the cache gained frames.
async fn fill_one_gap(
    shared: Rc<Shared>,
    target: FrameId,
    scrub_epoch: u64,
) -> Result<bool, MediaError> {
    let start_playhead = playhead_and_last(&shared).0;
    let key = shared
        .player
        .borrow()
        .track()
        .index()
        .nearest_keyframe(target)
        .unwrap_or(target);

    let cancelled = || {
        if shared.scrub_epoch.get() != scrub_epoch
            || shared.fill_mode.get() == CacheFillMode::Play
            || shared.player.borrow().state().playing
            || shared.hold_densify_for_exact.get()
        {
            return true;
        }
        let (playhead, last) = playhead_and_last(&shared);
        if playhead.abs_diff(start_playhead) < 8 {
            return false;
        }
        match shared.dense_cache.borrow().next_gap_frame(playhead, last) {
            Some(n) => {
                let n_key = shared
                    .player
                    .borrow()
                    .track()
                    .index()
                    .nearest_keyframe(n)
                    .unwrap_or(n);
                n_key != key && n.abs_diff(playhead) + 1 < target.abs_diff(playhead)
            }
            None => false,
        }
    };
    if cancelled() {
        return Err(MediaError::Cancelled);
    }
    {
        let index = shared.player.borrow().track().index().clone();
        let ids = index.gop_frame_ids(key).unwrap_or_default();
        // Only skip when this GOP is fully resident — do not bail just because
        // `target` exists (holes elsewhere in the GOP must still densify).
        // If the gap `target` is still missing, the complete check is stale /
        // inconsistent — fall through and decode.
        let cache = shared.dense_cache.borrow();
        if !ids.is_empty()
            && cache.gop_complete(key, &ids)
            && cache.has_frame(target, key)
        {
            // Gap already closed — count as progress so idle_passes does not trip.
            return Ok(true);
        }
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

    // Stream into dense as each remapped frame emerges so the ready strip
    // paints per-frame (not one tick-burst after the whole GOP).
    // Progress ≠ frame_count growth: at capacity, evict+insert keeps len stable.
    let shared_for_cache = Rc::clone(&shared);
    let newly = std::rc::Rc::new(std::cell::Cell::new(0u32));
    let newly_cb = Rc::clone(&newly);
    let mut on_frame = move |id: FrameId, video: OwnedVideoFrame| {
        let Ok(mut cache) = shared_for_cache.dense_cache.try_borrow_mut() else {
            drop(video);
            return;
        };
        if cache.insert_frame(id, video) {
            newly_cb.set(newly_cb.get().saturating_add(1));
        }
    };
    let decoded = decoder
        .decode_gop(track.as_ref(), key, &cancelled, &mut on_frame)
        .await;
    match decoded {
        Ok(()) => {
            *shared.scrub_decoder.borrow_mut() = Some(decoder);
            let got_target = shared.dense_cache.borrow().has_frame(target, key);
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
