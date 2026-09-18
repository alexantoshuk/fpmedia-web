//! High-level browser player: demux + WebCodecs + canvas.
//!
//! Present and decode share the player via interior mutability so an rAF
//! present loop never waits on `prefetch_one` (mediabunny-style split).

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::rc::Rc;

use web_sys::{CanvasRenderingContext2d, HtmlCanvasElement};

use crate::MediaClock;
use crate::decode::audio_output::AudioOutput;
use crate::decode::canvas::{canvas_2d, configure_canvas_for_video, draw_frame_with_context};
use crate::decode::frame_cache::{
    DEFAULT_MAX_THUMB_GOPS, DEFAULT_MAX_THUMBS, ScrubBlitLru, ScrubChunkStore, ThumbCache,
};
use crate::decode::owned_video_frame::OwnedVideoFrame;
use crate::decode::scrub_encode::{EncodeCanvasSlot, ScrubChunk, draw_image_bitmap};
use crate::decode::webcodecs::WebCodecsDecoder;
use crate::error::MediaError;
use crate::player::{Player, PlayerState};
use crate::types::{
    AudioMetadata, FrameId, GopProfile, ScrubPolicy, VideoMetadata, coalesce_frame_runs,
    scrub_blit_frame_budget,
};

/// Live VF ahead of the playhead (play prefetch + Next hits).
/// Keep small — each slot pins a `VideoFrame`; 16 + in-flight decode starved
/// Chromium's pool on DRYP play (`flush/idle` mid-GOP with `decodeQ≈12`).
/// Pair with [`STEP_BEHIND_CAP`] (ring ≈ 1 displayed + 8 behind + 4 ahead).
pub const PRESENTATION_QUEUE_CAP: usize = 4;
/// Live VF older than the playhead for Prev without key→N (step/scrub ring).
/// mediabunny reverse always restarts from key unless samples are retained —
/// this is the SampleCursor-lite behind window (`media-sink.ts` flush on rewind).
/// **Play must not keep this ring** — 1+8+4 + held≈8 pins ~21 VideoFrames and
/// starves Chromium's pool (underrun hold ≈0.77× realtime on DRYP). Forward play
/// uses cap 0; Pause/scrub restores [`STEP_BEHIND_CAP`].
pub const STEP_BEHIND_CAP: usize = 8;
/// Behind ring while `CacheFillMode::Play` — drop aged VF immediately.
const STEP_BEHIND_CAP_PLAY: usize = 0;
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
    /// Paused/scrub: fill gaps near playhead (predictive along scrub dir when set).
    Scrub,
}

struct Shared {
    player: RefCell<Player>,
    /// Ahead of playhead (presentation order). Cap [`PRESENTATION_QUEUE_CAP`].
    presentation: RefCell<VecDeque<QueuedFrame>>,
    /// VF currently matching canvas / committed playhead (not yet in behind).
    displayed: RefCell<Option<QueuedFrame>>,
    /// Older than displayed, oldest at front. Cap [`STEP_BEHIND_CAP`].
    step_behind: RefCell<VecDeque<QueuedFrame>>,
    /// Exact scrub store (resident markers) — densify / seek / step gap geometry.
    scrub_chunks: RefCell<ScrubChunkStore>,
    /// Live blit LRU for scrub present (`createImageBitmap`).
    scrub_blit: RefCell<ScrubBlitLru>,
    /// Reusable OffscreenCanvas for VideoFrame → ImageBitmap.
    encode_canvas: EncodeCanvasSlot,
    /// Filmstrip thumbs (caller-sized); separate budget from scrub chunks.
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
    /// Play underrun holds (clock freeze edges) — diagnose ~0.9× realtime.
    underrun_holds: Cell<u64>,
    /// Accumulated wall ms spent with media clock frozen for underrun.
    underrun_hold_ms: Cell<f64>,
    /// `performance.now` when the current underrun hold started (`None` = not holding).
    underrun_hold_since_ms: Cell<Option<f64>>,
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
    /// Scrub drag direction for predictive densify: `+1` ahead, `-1` behind, `0` neutral.
    scrub_dir: Cell<i8>,
    /// Last aim seen while updating `scrub_dir` (drag samples only).
    prev_scrub_aim: Cell<Option<FrameId>>,
    /// Optional AAC (etc.) sink — drives [`MediaClock`] when present.
    audio: RefCell<Option<Rc<AudioOutput>>>,
    clock: RefCell<MediaClock>,
    /// Wall `performance.now()/1000` origin while playing without audio.
    wall_origin: Cell<Option<f64>>,
}

/// Why a frame is marked ready on the demo timeline strip.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReadyFrameKind {
    /// Scrub store marker (coverage / gap geometry). May lack live blit.
    Covered,
    /// Live densify `ImageBitmap` (step-ready).
    Exact,
}

/// Paint order for overlapping strips (Covered under Exact).
fn ready_kind_paint_order(kind: ReadyFrameKind) -> u8 {
    match kind {
        ReadyFrameKind::Covered => 0,
        ReadyFrameKind::Exact => 1,
    }
}

/// One ready span for UI timeline marks (`frame`..=`end`, inclusive).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReadyFrameMark {
    /// Inclusive start (presentation frame).
    pub frame: FrameId,
    /// Inclusive end (`end >= frame`). Single tick when equal to `frame`.
    pub end: FrameId,
    pub kind: ReadyFrameKind,
}

impl ReadyFrameMark {
    pub fn run(lo: FrameId, hi: FrameId, kind: ReadyFrameKind) -> Self {
        Self {
            frame: lo,
            end: hi.max(lo),
            kind,
        }
    }
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
    /// Underrun clock-freeze edges since last [`WebPlayer::play`].
    pub underrun_holds: u64,
    /// Wall ms spent frozen for underrun since last play (includes in-progress hold).
    pub underrun_hold_ms: f64,
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
            underrun_holds: 0,
            underrun_hold_ms: 0.0,
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
        let opened = crate::open_mp4_media_bytes(bytes)?;
        let video = opened.video.ok_or(MediaError::NoVideoTrack)?;
        Self::from_tracks(video, opened.audio, canvas).await
    }

    /// Progressive open from a ranged [`crate::MediaSource`] (Blob / memory).
    pub async fn open_mp4_source(
        source: std::sync::Arc<dyn crate::MediaSource>,
        canvas: HtmlCanvasElement,
    ) -> Result<Self, MediaError> {
        let opened = crate::open_mp4_media_source_cached(
            source,
            crate::OpenMp4Options {
                verify_avc_sync: true,
            },
        )
        .await?;
        let video = opened.video.ok_or(MediaError::NoVideoTrack)?;
        Self::from_tracks(video, opened.audio, canvas).await
    }

    /// Progressive open from a browser [`web_sys::Blob`] / `File`.
    pub async fn open_blob(
        blob: web_sys::Blob,
        canvas: HtmlCanvasElement,
    ) -> Result<Self, MediaError> {
        let source: std::sync::Arc<dyn crate::MediaSource> =
            std::sync::Arc::new(crate::BlobSource::new(blob));
        Self::open_mp4_source(source, canvas).await
    }

    /// Build a player around an already-demuxed track (host can show stage status).
    pub async fn from_track(
        track: crate::demux::Mp4VideoTrack,
        canvas: HtmlCanvasElement,
    ) -> Result<Self, MediaError> {
        Self::from_tracks(track, None, canvas).await
    }

    /// Video track plus optional audio (same container).
    pub async fn from_tracks(
        track: crate::demux::Mp4VideoTrack,
        audio: Option<crate::demux::Mp4AudioTrack>,
        canvas: HtmlCanvasElement,
    ) -> Result<Self, MediaError> {
        let player = Player::from_track(track)?;
        Self::from_player(player, audio, canvas).await
    }

    async fn from_player(
        player: Player,
        audio_track: Option<crate::demux::Mp4AudioTrack>,
        canvas: HtmlCanvasElement,
    ) -> Result<Self, MediaError> {
        let meta = player.metadata().clone();
        configure_canvas_for_video(&canvas, meta.width, meta.height)?;
        let decoder = WebCodecsDecoder::create(&meta).await?;
        let ctx = canvas_2d(&canvas)?;
        let max_frames = meta.scrub_cache_frame_budget();
        let audio = match audio_track {
            Some(t) => match AudioOutput::create(t).await {
                Ok(a) => {
                    web_sys::console::log_1(
                        &format!(
                            "webmedia: audio track ready ({}, {} Hz, {} ch)",
                            a.metadata().codec.codec_string.as_deref().unwrap_or("?"),
                            a.metadata().sample_rate,
                            a.metadata().number_of_channels
                        )
                        .into(),
                    );
                    Some(Rc::new(a))
                }
                Err(e) => {
                    web_sys::console::warn_1(&format!("webmedia: audio disabled ({e})").into());
                    None
                }
            },
            None => {
                web_sys::console::log_1(&"webmedia: no audio track".into());
                None
            }
        };
        let shared = Rc::new(Shared {
            player: RefCell::new(player),
            presentation: RefCell::new(VecDeque::new()),
            displayed: RefCell::new(None),
            step_behind: RefCell::new(VecDeque::new()),
            scrub_chunks: RefCell::new(ScrubChunkStore::new(max_frames)),
            scrub_blit: RefCell::new(ScrubBlitLru::new(1)),
            encode_canvas: EncodeCanvasSlot::default(),
            thumb_cache: RefCell::new(ThumbCache::new(DEFAULT_MAX_THUMB_GOPS, DEFAULT_MAX_THUMBS)),
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
            underrun_holds: Cell::new(0),
            underrun_hold_ms: Cell::new(0.0),
            underrun_hold_since_ms: Cell::new(None),
            restarting: Cell::new(false),
            hold_densify_for_exact: Cell::new(false),
            preparing_loop: Cell::new(false),
            op_epoch: Cell::new(0),
            decoder_reset: Cell::new(false),
            fill_stalled_aim: Cell::new(None),
            scrub_dir: Cell::new(0),
            prev_scrub_aim: Cell::new(None),
            audio: RefCell::new(audio),
            clock: RefCell::new(MediaClock::new()),
            wall_origin: Cell::new(None),
        });
        apply_clip_budgets(&shared);
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

    pub fn audio_metadata(&self) -> Option<AudioMetadata> {
        self.shared
            .audio
            .borrow()
            .as_ref()
            .map(|a| a.metadata().clone())
    }

    pub fn has_audio(&self) -> bool {
        self.shared.audio.borrow().is_some()
    }

    pub fn set_volume(&self, volume: f64) {
        if let Some(a) = self.shared.audio.borrow().as_ref() {
            a.set_volume(volume);
        }
    }

    pub fn volume(&self) -> f64 {
        self.shared
            .audio
            .borrow()
            .as_ref()
            .map(|a| a.volume())
            .unwrap_or(0.0)
    }

    pub fn set_muted(&self, muted: bool) {
        if let Some(a) = self.shared.audio.borrow().as_ref() {
            a.set_muted(muted);
        }
    }

    pub fn muted(&self) -> bool {
        self.shared
            .audio
            .borrow()
            .as_ref()
            .map(|a| a.muted())
            .unwrap_or(true)
    }

    /// Media timeline seconds (audio `AudioContext` when present, else wall).
    pub fn media_time(&self) -> f64 {
        if let Some(a) = self.shared.audio.borrow().as_ref()
            && a.is_playing()
        {
            return a.media_time();
        }
        let elapsed = self
            .shared
            .wall_origin
            .get()
            .and_then(|origin| performance_now_ms().map(|now| now / 1000.0 - origin));
        self.shared.clock.borrow().media_time(elapsed)
    }

    /// Resume `AudioContext` while the Play click gesture is still warm.
    /// Call **before** long `prime_playback` awaits.
    pub async fn resume_audio_context(&self) -> Result<(), MediaError> {
        let audio = self.shared.audio.borrow().as_ref().map(Rc::clone);
        if let Some(a) = audio {
            a.resume_context().await?;
        }
        Ok(())
    }

    /// Arm the media clock after `prime_playback` (host play path).
    ///
    /// With an audio track this starts the decode/schedule pump (mediabunny
    /// `play` + `runAudioIterator`). Without audio, uses wall `performance`.
    pub async fn start_media_clock(&self, media_time_secs: f64) -> Result<(), MediaError> {
        self.finish_underrun_hold_timer();
        self.shared.clock.borrow_mut().start(media_time_secs);
        let audio = self.shared.audio.borrow().as_ref().map(Rc::clone);
        if let Some(a) = audio {
            self.shared.wall_origin.set(None);
            a.start_spawned(media_time_secs).await?;
        } else if let Some(now) = performance_now_ms() {
            self.shared.wall_origin.set(Some(now / 1000.0));
        }
        Ok(())
    }

    /// Snap the media clock (pause / seek). Stops audio nodes if attached.
    pub fn stop_media_clock(&self) -> f64 {
        let t = if let Some(a) = self.shared.audio.borrow().as_ref() {
            a.stop()
        } else {
            self.media_time()
        };
        self.shared.wall_origin.set(None);
        self.shared.clock.borrow_mut().snap(t);
        t
    }

    /// Freeze the media clock while the presentation queue is empty.
    ///
    /// Prefer this over `skip_behind` when Chromium decode briefly lags the
    /// audio clock (DRYP: skip spiral → `skipped≈242` then End before EOF).
    /// Keeps `playing` true; host resumes via [`Self::start_media_clock`] once
    /// prefetch has frames again.
    pub fn hold_media_clock_for_underrun(&self) -> f64 {
        self.note_underrun_hold_start();
        self.stop_media_clock()
    }

    fn note_underrun_hold_start(&self) {
        if self.shared.underrun_hold_since_ms.get().is_some() {
            return;
        }
        self.shared
            .underrun_holds
            .set(self.shared.underrun_holds.get().saturating_add(1));
        self.shared.underrun_hold_since_ms.set(performance_now_ms());
    }

    fn finish_underrun_hold_timer(&self) {
        let Some(since) = self.shared.underrun_hold_since_ms.take() else {
            return;
        };
        if let Some(now) = performance_now_ms() {
            let dt = (now - since).max(0.0);
            self.shared
                .underrun_hold_ms
                .set(self.shared.underrun_hold_ms.get() + dt);
        }
    }

    /// Hold stats including an in-progress freeze (for live pipeline UI).
    fn underrun_hold_ms_now(&self) -> f64 {
        let mut total = self.shared.underrun_hold_ms.get();
        if let (Some(since), Some(now)) = (
            self.shared.underrun_hold_since_ms.get(),
            performance_now_ms(),
        ) {
            total += (now - since).max(0.0);
        }
        total
    }

    pub fn state(&self) -> PlayerState {
        self.shared.player.borrow().state()
    }

    pub fn canvas(&self) -> &HtmlCanvasElement {
        &self.shared.canvas
    }

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
    fn repaint_after_buffer_resize(&self) -> Result<(), MediaError> {
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

    pub fn is_playing(&self) -> bool {
        self.state().playing
    }

    pub fn play(&self) {
        self.shared.fill_mode.set(CacheFillMode::Play);
        // Cancel densify hard — do **not** rearm `scrub_pending` (Play must not
        // keep filling gaps). Keep scrub *markers*; drop live ImageBitmaps so Play
        // decode is not fighting densify GPU residency (fullscreen / after warm).
        self.cancel_ops();
        self.abort_scrub_fill(false);
        self.clear_scrub_direction();
        if let Ok(mut blit) = self.shared.scrub_blit.try_borrow_mut() {
            blit.clear();
        }
        // Free Chromium VideoFrame pool for prefetch — step ring is Pause/Prev only.
        self.clear_step_behind_ring();
        self.shared.player.borrow_mut().play();
        self.shared.frames_skipped.set(0);
        self.shared.last_behind_ms.set(None);
        self.shared.underrun_holds.set(0);
        self.shared.underrun_hold_ms.set(0.0);
        self.shared.underrun_hold_since_ms.set(None);
    }

    pub fn pause(&self) {
        // Leaving play: cancel decode + scrub fill epoch (pending latch restarts fill).
        // Already paused: do not bump scrub_epoch (UI steps call pause every time).
        let was_playing = self.shared.player.borrow().state().playing;
        if was_playing {
            self.cancel_ops_and_scrub_fill();
            let _ = self.stop_media_clock();
            self.finish_underrun_hold_timer();
        }
        self.shared.player.borrow_mut().pause();
        self.clear_loop_prime();
        // Never leave densify blocked behind a settle hold after pause / transport stop.
        // (`prepare_scrub_settle` + cancelled settle, or play-after-scrub skipping arm,
        // could latch hold/pending with no running fill task.)
        self.shared.hold_densify_for_exact.set(false);
        // Same pipeline as scrub drag / hold / release.
        self.enter_scrub_cache_pipeline(self.state().frame);
        if was_playing {
            // Cancelled fill may still have `scrub_busy` until its finally runs;
            // schedule only latches pending then. Kick again after a turn so pause
            // densify is not lost when the busy race clears without want_again.
            self.shared.scrub_pending.set(true);
            let shared = Rc::clone(&self.shared);
            wasm_bindgen_futures::spawn_local(async move {
                yield_event_loop().await;
                if shared.hold_densify_for_exact.get()
                    || shared.fill_mode.get() == CacheFillMode::Play
                    || shared.player.borrow().state().playing
                    || !shared.player.borrow().metadata().scrub_policy().densify
                {
                    return;
                }
                if shared.scrub_busy.get() {
                    shared.scrub_pending.set(true);
                    return;
                }
                if next_gap_target(&shared).is_none() {
                    return;
                }
                shared.scrub_busy.set(true);
                shared.scrub_pending.set(false);
                let again = Rc::clone(&shared);
                let _ = scrub_cache_fill_task(again).await;
            });
        }
    }

    /// Aim + park only — no densify kick. Used while the main decoder owns an
    /// exact seek/scrub (`restarting`) so the scrub decoder does not compete for
    /// the HW session until the finger frame is on canvas.
    fn aim_scrub_cache(&self, focus: FrameId) {
        self.shared.fill_mode.set(CacheFillMode::Scrub);
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
    /// While [`Shared::hold_densify_for_exact`] (scrub settle / sync seek), densify
    /// is deferred so exact `goto_frame` owns HW; soft-drag scrub keeps densify on.
    pub fn enter_scrub_cache_pipeline(&self, focus: FrameId) {
        self.aim_scrub_cache(focus);
        if self.scrub_policy().densify {
            self.schedule_scrub_cache_fill();
        }
    }

    fn scrub_policy(&self) -> ScrubPolicy {
        self.shared.player.borrow().metadata().scrub_policy()
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
        self.clear_scrub_direction();
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
        // Take under a short borrow — do not `if let … borrow_mut().take()` (RefMut
        // spans the body; `reset` / Drop re-entry panics).
        let taken = self.shared.scrub_decoder.borrow_mut().take();
        if let Some(mut d) = taken {
            let _ = d.reset();
        }
    }

    /// Update predictive densify direction from a drag sample (`sign(aim − prev)`).
    fn note_scrub_direction(&self, aim: FrameId) {
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
    fn clear_scrub_direction(&self) {
        self.shared.scrub_dir.set(0);
        self.shared.prev_scrub_aim.set(None);
    }

    /// Pointer-up settle entry from the UI host — before the async op runs.
    ///
    /// Must not call [`Self::note_scrub_frame`] / densify kick: that starved exact
    /// on Wing It–class long GOPs (playhead far, `eng` stuck at 0, `decodeQ≈10`).
    pub fn prepare_scrub_settle(&self, frame: FrameId) {
        self.abort_scrub_fill(true);
        self.shared.hold_densify_for_exact.set(true);
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

    /// Drag sample entry — clear exact-hold so densify can run under the finger.
    ///
    /// `prepare_scrub_settle` may have latched hold; if settle is superseded by a
    /// new drag before `scrub_to` clears it, densify stayed dead until Pause.
    pub fn clear_densify_hold(&self) {
        if !self.shared.hold_densify_for_exact.get() {
            return;
        }
        self.shared.hold_densify_for_exact.set(false);
        self.schedule_scrub_cache_fill();
    }

    /// Snap the paused media clock to the committed playhead.
    ///
    /// Scrub/seek update `Player` frame but used to leave `MediaClock` at the
    /// prior pause/EOF time. PlayPrime must not let rAF see that stale time
    /// (Ended → stop_playback mid-prime); snapping also keeps `media_time()`
    /// honest while paused. mediabunny: seek updates `playbackTime`.
    fn snap_media_clock_to_playhead(&self) {
        let t = self.state().timestamp.seconds;
        self.shared.wall_origin.set(None);
        self.shared.clock.borrow_mut().snap(t);
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
            presentation_cap: PRESENTATION_QUEUE_CAP,
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
            .ok_or(MediaError::FrameOutOfRange(
                frame,
                player.state().frame_count,
            ))?;
        if entry.duration_ticks == 0 || entry.timescale == 0 {
            return Err(MediaError::MissingFrameDuration(frame));
        }
        Ok(entry.duration_ticks as f64 / entry.timescale as f64)
    }

    pub async fn seek_frame(&self, frame: FrameId) -> Result<(), MediaError> {
        let epoch = self.begin_op();
        // Exact VF — abort densify and hold so scrub HW cannot starve the cascade.
        // Densify blit is for scrub/step warm hits; seek always decodes VF.
        self.abort_scrub_fill(true);
        self.shared.hold_densify_for_exact.set(true);
        self.shared.restarting.set(true);
        let result = async {
            self.shared.player.borrow_mut().pause();
            self.clear_loop_prime();
            self.clear_step_ring();
            self.clear_presentation();
            self.aim_scrub_cache(frame);
            // Keep last canvas until exact decode paints (same as scrub settle).
            self.goto_frame_with_interim(frame, epoch).await
        }
        .await;
        self.shared.restarting.set(false);
        self.shared.hold_densify_for_exact.set(false);
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

    /// Scrub pointer sample (drag / hold). Same cache pipeline as pause + release.
    pub fn note_scrub_frame(&self, frame: FrameId) {
        self.note_scrub_direction(frame);
        self.enter_scrub_cache_pipeline(frame);
    }

    /// Async timeline scrub (local scrub policy; settle always exact VF).
    ///
    /// - **Drag** (`settle=false`): densify while moving. [`ScrubDragMode::Soft`]
    ///   (short-IP / All-Intra) paints blit/preview only — no key→N. Exact drag
    ///   (Long) may `goto_frame`. Gap pick follows scrub direction (predictive densify).
    /// - **Settle** (`settle=true`, pointer up): abort densify, then one key→exact
    ///   cascade (`goto_frame_with_interim`) so canvas gets decode `VideoFrame`
    ///   (not a soft still). Interim preview may blit cache; pruned frames fill
    ///   densify markers without blit.
    pub async fn scrub_to(&self, frame: FrameId, settle: bool) -> Result<(), MediaError> {
        let epoch = self.begin_op();
        let soft_drag = self.scrub_policy().soft_drag();
        if settle {
            // Host may already have called [`Self::prepare_scrub_settle`]; reinforce.
            self.abort_scrub_fill(true);
            self.shared.hold_densify_for_exact.set(true);
            self.clear_scrub_direction();
            self.shared.restarting.set(true);
            self.aim_scrub_cache(frame);
        } else {
            self.shared.hold_densify_for_exact.set(false);
            self.shared.restarting.set(true);
            self.note_scrub_direction(frame);
            // Drag: kick densify immediately so the strip fills while the finger moves.
            self.enter_scrub_cache_pipeline(frame);
        }
        let result = async {
            self.shared.player.borrow_mut().pause();
            self.clear_loop_prime();

            if settle {
                // Keep the last drag frame on canvas until exact decode VF paints.
                // Do **not** blit key/preview from store — key is often far from
                // target and flashes a wrong frame; store is low-q drag trail only.
                // Reinforce ring clear when host skipped [`Self::prepare_scrub_settle`].
                self.clear_step_ring();
                self.clear_presentation();
                self.goto_frame_with_interim(frame, epoch).await
            } else {
                if self.present_from_cache(frame).await? {
                    self.shared.player.borrow_mut().commit_frame(frame)?;
                    return Ok(());
                }

                // Drag: may paint key/neighbor for interim soft preview.
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
                if soft_drag {
                    // Soft-drag: leave exact for settle; densify keeps warming.
                    // Canvas stays on blit/preview; eng commits only on blit hit.
                    return Ok(());
                }
                self.goto_frame(frame, epoch).await
            }
        }
        .await;
        self.shared.restarting.set(false);
        self.shared.hold_densify_for_exact.set(false);
        // Cancelled (superseded drag / seek): do not kick densify — a following
        // PlayPrime must keep a warm main decoder when possible.
        if !self.is_current(epoch) {
            return Err(MediaError::Cancelled);
        }
        if settle {
            // Densify is armed by the host *after* the drain loop can see a queued
            // PlayPrime — otherwise invalidate + scrub HW kick cause play-after-scrub
            // lag on long GOPs (Wing It). Drag still densifies below.
            // Do **not** `schedule_scrub_cache_fill` here when Play is pending — host
            // skips `arm_scrub_densify_after_exact` on purpose; Pause restarts fill.
        } else {
            self.enter_scrub_cache_pipeline(frame);
        }
        result?;
        self.snap_media_clock_to_playhead();
        Ok(())
    }

    /// After exact settle / sync seek: idle main decoder, then densify.
    ///
    /// Call only when play is **not** about to start — PlayPrime wants the warm
    /// cascade left by seekTo, not a cold `invalidate` + competing scrub decoder.
    pub fn arm_scrub_densify_after_exact(&self, focus: FrameId) {
        self.invalidate_decoder();
        self.enter_scrub_cache_pipeline(focus);
    }

    /// After sequential Next/Prev: densify without killing the warm main decoder
    /// / step VF ring (`mediabunny: samples()` keep iterator; no flush per step).
    fn arm_scrub_densify_after_step(&self, focus: FrameId) {
        self.enter_scrub_cache_pipeline(focus);
    }

    /// Best interim canvas frame while exact `target` is missing from dense cache.
    ///
    /// Prefer a cached neighbor when `|cached − target| ≤ |key − target|` (tie →
    /// cache: already decoded, no key jump). Otherwise the nearest sync key.
    fn scrub_preview_frame(&self, target: FrameId) -> Option<FrameId> {
        let key = self.key_for(target);
        let cached = self.shared.scrub_chunks.borrow().nearest_frame(target);
        crate::scrub_preview_choice(target, key, cached)
    }

    /// Decode/draw `frame` for canvas preview without updating the committed playhead.
    ///
    /// Cache hits draw in place. Key (uncached) previews decode without inserting
    /// into dense — orphan syncs must not punch holes outside the scrub window.
    async fn draw_frame_unccommitted(&self, frame: FrameId, epoch: u64) -> Result<(), MediaError> {
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
    async fn step_soft_goto(&self, frame: FrameId, fill_behind: bool) -> Result<(), MediaError> {
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
                // Play path: present + Exact ingest (same as present_due).
                self.present_play_frame(queued.frame, queued.video)?;
                self.touch_playhead(queued.frame);
                self.shared.player.borrow_mut().commit_frame(queued.frame)?;
                return Ok(self.has_more_playback());
            }
        }

        self.clear_presentation();
        // Soft catch-up (mediabunny `updateNextFrame` loop): keep warm decode +
        // dense when the target stays in the same GOP. Hard invalidate only when
        // catch-up crosses into a later GOP — a full key→N after wipe was Chrome
        // DRYP's stutter loop (play a few → clear strip → flush/idle mid-GOP).
        // Do **not** `clear_scrub_store` here — scrub residency is unrelated to
        // underrun and wiping it was the visible "cache vanishes" symptom.
        // Behind ring is stale past the catch-up target — drop it.
        self.clear_step_ring();
        let hard_seek = !self.is_all_intra() && {
            let player = self.shared.player.borrow();
            let index = player.track().index();
            match (index.nearest_keyframe(next), index.nearest_keyframe(target)) {
                (Some(nk), Some(tk)) => tk != nk,
                _ => true,
            }
        };
        if hard_seek {
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
            self.clear_step_ring();
            return Ok(false);
        }
        self.clear_presentation();
        self.clear_step_ring();
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
        // Scrub store is markers + blit — play re-decodes into the presentation
        // queue. Do **not** trim scrub residency on Play — eviction is budget /
        // farthest-from-aim only (Pause densify paid for that trail).
        self.fill_presentation_queue(epoch).await
    }

    pub fn frame_timestamp_seconds(&self, frame: FrameId) -> Result<f64, MediaError> {
        let player = self.shared.player.borrow();
        let entry = player
            .track()
            .index()
            .get(frame)
            .ok_or(MediaError::FrameOutOfRange(
                frame,
                player.state().frame_count,
            ))?;
        Ok(entry.composition_timestamp().seconds)
    }

    pub fn presentation_len(&self) -> usize {
        self.shared.presentation.borrow().len()
    }

    /// True underrun: presentation empty while the next frame is already due.
    ///
    /// Used to freeze the media clock (not for `skip_behind`). Distinct from a
    /// healthy empty queue while waiting for the next frame's deadline.
    pub fn play_underrun_hold(&self) -> bool {
        if !self.is_playing() || self.shared.restarting.get() {
            return false;
        }
        if self.presentation_len() > 0 {
            return false;
        }
        let state = self.state();
        let next = state.frame.saturating_add(1);
        if next >= state.frame_count {
            return false;
        }
        let Ok(start) = self.frame_timestamp_seconds(next) else {
            return false;
        };
        self.media_time() >= start
    }

    /// Media-clock seconds ahead of the next needed frame while the presentation
    /// queue is empty (play underrun). `None` if not underrunning or lag below
    /// [`BIG_LAG_SECONDS`]. Prefer [`Self::play_underrun_hold`] + clock freeze for
    /// Chrome; `skip_behind` remains for explicit catch-up.
    pub fn play_underrun_behind(&self) -> Option<f64> {
        if !self.play_underrun_hold() {
            return None;
        }
        let state = self.state();
        let next = state.frame.saturating_add(1);
        let start = self.frame_timestamp_seconds(next).ok()?;
        let behind = self.media_time() - start;
        (behind >= BIG_LAG_SECONDS).then_some(behind)
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
            // Do not End while decoded frames remain — audio can reach duration
            // while video is still draining the presentation queue after an
            // underrun hold (DRYP stopped at 347/368 with empty error).
            if self.presentation_len() > 0 {
                // Fall through to present remaining due frames at EOF.
            } else if self.is_looping() {
                return Ok(DuePresent::NeedsLoop);
            } else {
                self.shared.player.borrow_mut().pause();
                self.clear_presentation();
                self.clear_step_ring();
                return Ok(DuePresent::Ended);
            }
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
            // Play: Exact ingest of presented frame; densify stays off.
            self.present_play_frame(queued.frame, queued.video)?;
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
        self.clear_step_ring();
        self.clear_scrub_store();
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
            match self.decode_frame(&track, frame, epoch).await {
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
            self.clear_step_ring();
            self.clear_scrub_store();
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
        let video = match self.decode_frame(&track, next, epoch).await {
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
            .push_back(QueuedFrame { frame: next, video });
        Ok(true)
    }

    pub async fn prefetch_until(&self, deadline_ms: f64) -> Result<usize, MediaError> {
        let mut decoded = 0usize;
        while self.is_playing() && self.presentation_len() < PRESENTATION_QUEUE_CAP {
            if let Some(now) = performance_now_ms()
                && now + PREFETCH_DEADLINE_SLACK_MS >= deadline_ms
            {
                break;
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
        let next_entry = index.get(next).ok_or(MediaError::FrameOutOfRange(
            next,
            player.state().frame_count,
        ))?;
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

        if behind_seconds >= BIG_LAG_SECONDS
            && let Some(key) = index.nearest_keyframe(target)
            && key >= next
        {
            return Ok(key);
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
                    self.present_for_mode(queued.frame, queued.video)?;
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
                        self.present_for_mode(queued.frame, queued.video)?;
                        self.touch_playhead(queued.frame);
                        self.shared.player.borrow_mut().commit_frame(queued.frame)?;
                        return Ok(());
                    }
                } else {
                    drop(presentation);
                    self.clear_presentation();
                    self.clear_step_ring();
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

    /// Drop live step VF ring (displayed + behind). Seek / scrub settle / teardown.
    fn clear_step_ring(&self) {
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

    fn take_presentation_front(&self, frame: FrameId) -> Option<OwnedVideoFrame> {
        let mut presentation = self.shared.presentation.borrow_mut();
        if presentation.front().is_some_and(|f| f.frame == frame) {
            presentation.pop_front().map(|q| q.video)
        } else {
            None
        }
    }

    fn push_presentation_front(&self, q: QueuedFrame) {
        let mut presentation = self.shared.presentation.borrow_mut();
        presentation.retain(|x| x.frame != q.frame);
        presentation.push_front(q);
        while presentation.len() > PRESENTATION_QUEUE_CAP {
            if let Some(drop_q) = presentation.pop_back() {
                drop(drop_q.video);
            }
        }
    }

    fn take_step_behind(&self, frame: FrameId) -> Option<OwnedVideoFrame> {
        let mut behind = self.shared.step_behind.borrow_mut();
        if behind.back().is_some_and(|q| q.frame == frame) {
            return behind.pop_back().map(|q| q.video);
        }
        if let Some(i) = behind.iter().position(|q| q.frame == frame) {
            return behind.remove(i).map(|q| q.video);
        }
        None
    }

    fn push_step_behind(&self, q: QueuedFrame) {
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

    fn step_behind_cap(&self) -> usize {
        if self.shared.fill_mode.get() == CacheFillMode::Play {
            STEP_BEHIND_CAP_PLAY
        } else {
            STEP_BEHIND_CAP
        }
    }

    /// Drop all step-behind VFs (Play start — free decoder pool).
    fn clear_step_behind_ring(&self) {
        let drained: Vec<_> = self.shared.step_behind.borrow_mut().drain(..).collect();
        for old in drained {
            drop(old.video);
        }
    }

    fn trim_step_behind(&self) {
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
    fn install_displayed(&self, frame: FrameId, video: OwnedVideoFrame) -> Result<(), MediaError> {
        let prev = self.shared.displayed.borrow_mut().take();
        if let Some(old) = prev {
            if self.shared.fill_mode.get() == CacheFillMode::Play {
                // Play: close immediately — do not pin behind VFs (pool pressure).
                drop(old.video);
            } else {
                self.push_step_behind(old);
            }
        }
        self.install_displayed_no_retire(frame, video)
    }

    fn install_displayed_no_retire(
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
    fn exact_ingest_displayed_borrow(&self) {
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
    fn present_and_cache(&self, frame: FrameId, video: OwnedVideoFrame) -> Result<(), MediaError> {
        self.install_displayed(frame, video)?;
        self.exact_ingest_displayed_borrow();
        Ok(())
    }

    /// Play present: draw + keep VF in ring; Exact ingest when frames age out of behind.
    fn present_play_frame(&self, frame: FrameId, video: OwnedVideoFrame) -> Result<(), MediaError> {
        self.install_displayed(frame, video)?;
        Ok(())
    }

    /// Drop Exact scrub store + blit + thumbs (open teardown / loop restart).
    fn clear_scrub_store(&self) {
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
        if let Ok(mut thumbs) = self.shared.thumb_cache.try_borrow_mut() {
            thumbs.clear();
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
        if let Ok(mut cache) = self.shared.scrub_chunks.try_borrow_mut() {
            cache.touch_playhead(frame);
        }
        if let Ok(mut blit) = self.shared.scrub_blit.try_borrow_mut() {
            blit.set_playhead(frame);
        }
    }

    /// Park `video` into scrub residency: sync `ImageBitmap` + gap marker.
    ///
    /// `mediabunny:` CanvasSink — present from decoded sample / canvas pool, no
    /// intermediate compressed stills on the scrub critical path.
    fn ingest_scrub_frame(&self, frame: FrameId, video: OwnedVideoFrame, replace: bool) {
        ingest_scrub_frame_shared(&self.shared, frame, video, replace);
    }

    /// Present a densify-warm frame: **ImageBitmap only** (VF→canvas transfer).
    ///
    /// Store markers are gap geometry / strip only — never paint a still onto the
    /// playback canvas. Miss → `Ok(false)` so callers decode a real `VideoFrame`.
    async fn present_from_cache(&self, frame: FrameId) -> Result<bool, MediaError> {
        self.try_present_warm_blit(frame)
    }

    /// Present densify `ImageBitmap` only (VF→canvas transfer).
    ///
    /// Next/Prev and scrub-drag share this path after warm fill.
    fn try_present_warm_blit(&self, frame: FrameId) -> Result<bool, MediaError> {
        let Ok(mut blit) = self.shared.scrub_blit.try_borrow_mut() else {
            return Ok(false);
        };
        let Some(bitmap) = blit.get(frame) else {
            return Ok(false);
        };
        draw_image_bitmap(&self.shared.canvas, &self.shared.ctx, bitmap)?;
        Ok(true)
    }

    fn clear_loop_prime(&self) {
        let taken = self.shared.loop_prime.borrow_mut().take();
        if let Some(buf) = taken {
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
        // Take out so `reset()` does not hold the RefCell (JS re-entry / Drop).
        let taken = self.decoder.borrow_mut().take();
        if let Some(mut decoder) = taken {
            if decoder.reset().is_ok() {
                self.shared.decoder_reset.set(false);
            }
            *self.decoder.borrow_mut() = Some(decoder);
        }
    }

    /// Take the decoder out of the slot for the duration of an async decode so
    /// we never hold `RefMut` across `.await` (avoids freezes / panics).
    ///
    /// Waits briefly if another op still holds the decoder (quick Pause→Play:
    /// cancelled prefetch must put the decoder back before prime can run).
    async fn decode_frame(
        &self,
        track: &Rc<crate::demux::Mp4VideoTrack>,
        frame: FrameId,
        epoch: u64,
    ) -> Result<OwnedVideoFrame, MediaError> {
        self.decode_frame_inner(track, frame, epoch, None).await
    }

    async fn decode_frame_with_interim_cb(
        &self,
        track: &Rc<crate::demux::Mp4VideoTrack>,
        frame: FrameId,
        epoch: u64,
        on_interim: Box<dyn FnMut(FrameId, OwnedVideoFrame)>,
    ) -> Result<OwnedVideoFrame, MediaError> {
        self.decode_frame_inner(track, frame, epoch, Some(on_interim))
            .await
    }

    /// The decoder's packet pump outlives this call's borrow, so the track must
    /// be shared (`Rc`) rather than borrowed.
    async fn decode_frame_inner(
        &self,
        track: &Rc<crate::demux::Mp4VideoTrack>,
        frame: FrameId,
        epoch: u64,
        on_interim: Option<Box<dyn FnMut(FrameId, OwnedVideoFrame)>>,
    ) -> Result<OwnedVideoFrame, MediaError> {
        let mut decoder = self.take_decoder(epoch).await?;
        if self.shared.decoder_reset.get() {
            decoder.reset()?;
            self.shared.decoder_reset.set(false);
        }
        let cancelled = || !self.is_current(epoch);
        let result = match on_interim {
            Some(cb) => {
                decoder
                    .decode_frame_with_interim(track, frame, &cancelled, cb)
                    .await
            }
            None => decoder.decode_frame(track, frame, &cancelled).await,
        };
        let result = match result {
            // One hard reset + retry — orphan PTS after a raced invalidate.
            Err(MediaError::WebCodecs(msg))
                if msg.contains("flush/idle without pts=")
                    || msg.contains("decode pump stalled")
                    || msg.contains("exhausted GOP before pts=")
                    || msg.contains("decodeQueueSize backpressure timed out") =>
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
        // Wait for a peer that holds the slot (prefetch / seek / GOP fill). Yield
        // macrotasks — microtask-only spins can return busy before the holder
        // resumes. Cap by wall-clock: Chromium often clamps `setTimeout(0)` to
        // ~4ms, so a fixed 120-spin budget was only ~0.5s and false-fired
        // `decoder busy` under play load (Firefox was fine).
        // mediabunny: no shared slot — iterators use `terminated`; we cancel via
        // `op_epoch` and must keep waiting while the epoch is current
        // (`skills/leptos.md` Quick Pause→Play).
        const TAKE_DECODER_WAIT_MS: f64 = 5_000.0;
        let start = performance_now_ms().unwrap_or(0.0);
        loop {
            if !self.is_current(epoch) {
                return Err(MediaError::Cancelled);
            }
            let taken = self.decoder.borrow_mut().take();
            if let Some(decoder) = taken {
                return Ok(decoder);
            }
            let now = performance_now_ms().unwrap_or(start);
            if now - start >= TAKE_DECODER_WAIT_MS {
                break;
            }
            sleep_ms(0).await;
        }
        Err(MediaError::WebCodecs(
            "decoder busy (Pause/Play raced — try Play again)".into(),
        ))
    }

    async fn goto_frame(&self, frame: FrameId, epoch: u64) -> Result<(), MediaError> {
        let track = self.shared.player.borrow().track_rc();
        let video_frame = self.decode_frame(&track, frame, epoch).await?;
        if !self.is_current(epoch) {
            drop(video_frame);
            return Err(MediaError::Cancelled);
        }
        self.present_and_cache(frame, video_frame)?;
        self.shared.player.borrow_mut().commit_frame(frame)?;
        Ok(())
    }

    /// Exact seek that salvages pruned early frames into densify markers (no canvas blit).
    ///
    /// Canvas stays on the caller's static best cached/key preview until exact.
    /// One key→N pass warms the GOP so post-exact densify is cheap.
    async fn goto_frame_with_interim(&self, frame: FrameId, epoch: u64) -> Result<(), MediaError> {
        let track = self.shared.player.borrow().track_rc();
        let shared = Rc::clone(&self.shared);
        let on_interim: Box<dyn FnMut(FrameId, OwnedVideoFrame)> =
            Box::new(move |id: FrameId, video: OwnedVideoFrame| {
                ingest_scrub_frame_shared(&shared, id, video, false);
            });
        let video_frame = self
            .decode_frame_with_interim_cb(&track, frame, epoch, on_interim)
            .await?;
        if !self.is_current(epoch) {
            drop(video_frame);
            return Err(MediaError::Cancelled);
        }
        self.present_and_cache(frame, video_frame)?;
        self.shared.player.borrow_mut().commit_frame(frame)?;
        Ok(())
    }

    /// Prev miss: key→target cascade; park near-target pruned VF into the behind ring.
    ///
    /// `mediabunny: mediaSamplesAtTimestamps` restarts from key on rewind — we keep
    /// a capped behind window so the next few Prev hits avoid another cascade.
    async fn goto_frame_fill_behind(&self, frame: FrameId, epoch: u64) -> Result<(), MediaError> {
        let track = self.shared.player.borrow().track_rc();
        let shared = Rc::clone(&self.shared);
        let target = frame;
        let on_interim: Box<dyn FnMut(FrameId, OwnedVideoFrame)> =
            Box::new(move |id: FrameId, video: OwnedVideoFrame| {
                let floor = target.saturating_sub(STEP_BEHIND_CAP as FrameId);
                if id >= floor && id < target {
                    let mut behind = shared.step_behind.borrow_mut();
                    behind.retain(|x| x.frame != id);
                    match behind.iter().position(|x| x.frame > id) {
                        Some(i) => behind.insert(i, QueuedFrame { frame: id, video }),
                        None => behind.push_back(QueuedFrame { frame: id, video }),
                    }
                    while behind.len() > STEP_BEHIND_CAP {
                        if let Some(old) = behind.pop_front() {
                            drop(old.video);
                        }
                    }
                } else {
                    ingest_scrub_frame_shared(&shared, id, video, false);
                }
            });
        let video_frame = self
            .decode_frame_with_interim_cb(&track, frame, epoch, on_interim)
            .await?;
        if !self.is_current(epoch) {
            drop(video_frame);
            return Err(MediaError::Cancelled);
        }
        self.present_and_cache(frame, video_frame)?;
        self.shared.player.borrow_mut().commit_frame(frame)?;
        Ok(())
    }

    /// Play mode → draw + Exact ingest (no densify); scrub/seek → `present_and_cache`.
    fn present_for_mode(&self, frame: FrameId, video: OwnedVideoFrame) -> Result<(), MediaError> {
        if self.is_playing() && self.shared.fill_mode.get() == CacheFillMode::Play {
            self.present_play_frame(frame, video)
        } else {
            self.present_and_cache(frame, video)
        }
    }

    /// Draw without inserting (scrub preview — presentation queue owns ahead frames).
    fn present_only(&self, video: OwnedVideoFrame) -> Result<(), MediaError> {
        let result =
            draw_frame_with_context(&self.shared.canvas, &self.shared.ctx, video.as_web_sys());
        drop(video);
        result
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

    fn schedule_scrub_cache_fill(&self) {
        // Gate on play mode / densify-off policy (All-Intra).
        // Settle/seek: latch pending while exact owns HW (`hold_densify_for_exact`).
        if self.shared.fill_mode.get() == CacheFillMode::Play
            || self.is_playing()
            || !self.scrub_policy().densify
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
            || !self.scrub_policy().densify
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
}

impl Drop for WebPlayer {
    fn drop(&mut self) {
        self.clear_loop_prime();
        self.clear_presentation();
        self.clear_step_ring();
        self.clear_scrub_store();
    }
}

async fn scrub_cache_fill_task(shared: Rc<Shared>) -> Result<(), MediaError> {
    let mut scrub_epoch = shared.scrub_epoch.get();
    let result = async {
        let mut idle_passes = 0u8;
        loop {
            if shared.fill_mode.get() == CacheFillMode::Play
                || shared.player.borrow().state().playing
                || !shared.player.borrow().metadata().scrub_policy().densify
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
                    // Macrotask so paused rAF can refresh the ready strip.
                    sleep_ms(0).await;
                }
                Err(MediaError::Cancelled) => {
                    // Settle/seek hold — exit so scrub HW yields immediately
                    // (do not spin/sleep; was competing with exact after long drag).
                    if shared.hold_densify_for_exact.get() {
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
    let policy = meta.scrub_policy();
    let max_frames = meta.scrub_cache_frame_budget();
    shared.scrub_chunks.borrow_mut().set_max_frames(max_frames);
    // Aim-centered blit: MiB wallet → frame count from canvas buffer (RGBA).
    // `mediabunny:` CanvasSink width/height/fit + poolSize (sized, bounded VRAM).
    let buf_w = shared.canvas.width().max(1);
    let buf_h = shared.canvas.height().max(1);
    let blit_cap = scrub_blit_frame_budget(buf_w, buf_h, policy.blit_mib);
    shared.scrub_blit.borrow_mut().set_max(blit_cap);
}

/// Densify / scrub ingest: VF → sync `ImageBitmap` + resident marker.
///
/// `mediabunny:` CanvasSink canvas-pool present — keep live bitmaps, no still encode.
fn ingest_scrub_bitmap_only(
    shared: &Rc<Shared>,
    frame: FrameId,
    video: &web_sys::VideoFrame,
    target_w: u32,
    target_h: u32,
    replace: bool,
) -> bool {
    let (bitmap, _width, _height) = match shared
        .encode_canvas
        .with_encoder(|enc| enc.frame_to_bitmap(video, target_w.max(1), target_h.max(1)))
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
        blit.put(frame, bitmap);
        true
    } else {
        bitmap.close();
        if inserted_new {
            let _ = shared
                .scrub_chunks
                .try_borrow_mut()
                .map(|mut s| s.remove(frame));
        }
        false
    }
}

fn ingest_scrub_frame_shared(
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

fn playhead_and_last(shared: &Shared) -> (FrameId, FrameId) {
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
fn next_gap_target(shared: &Shared) -> Option<FrameId> {
    let (playhead, last) = playhead_and_last(shared);
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
    // Play keeps markers and clears blit — ImageBitmap holes must win over
    // marker `no_gaps`, or pause densify never re-warms the Exact strip.
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
        || !cache.gap_fill_armed(playhead, margin_left, margin_right)
    {
        return None;
    }
    cache.next_gap_frame(playhead, last, dir)
}

/// Densify the GOP containing `target`. Returns whether the cache gained frames.
///
/// Once a GOP decode starts, **finish it** even if the scrub aim moves — rapid
/// drag used to cancel mid-GOP (≥8 frames / other key) and left dense empty.
/// Aim only picks the *next* gap after this GOP completes. Still abort on settle
/// hold, play, or scrub-epoch bump.
async fn fill_one_gap(
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
            || shared.fill_mode.get() == CacheFillMode::Play
            || shared.player.borrow().state().playing
            || shared.hold_densify_for_exact.get()
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
        None => WebCodecsDecoder::create(&meta).await?,
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
