//! High-level browser player: demux + WebCodecs + canvas.
//!
//! Present and decode share the player via interior mutability so an rAF
//! present loop never waits on `prefetch_one` (mediabunny-style split).

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::rc::Rc;

use wasm_bindgen::closure::Closure;
use web_sys::{CanvasRenderingContext2d, HtmlCanvasElement, MessageEvent, Worker};

use crate::MediaClock;
use crate::decode::audio_output::AudioOutput;
use crate::decode::canvas::{
    canvas_2d, configure_canvas_for_video, device_pixel_ratio, draw_frame_with_context,
};
use crate::decode::frame_cache::{
    DEFAULT_MAX_THUMB_GOPS, DEFAULT_MAX_THUMBS, SCRUB_BLIT_LRU, ScrubBlitLru, ScrubChunkStore,
    ThumbCache,
};
use crate::decode::owned_video_frame::OwnedVideoFrame;
use crate::decode::play_encode_worker::{
    PlayWebpJob, parse_play_webp_reply, post_play_webp_job, revoke_worker_url,
    spawn_play_webp_worker,
};
use crate::decode::scrub_encode::{
    EncodeCanvasSlot, PENDING_WEBP_MIME, SCRUB_SKIP_PLAY_WEBP_DATA_URL, ScrubChunk,
    draw_image_bitmap, image_bitmap_from_chunk,
};
use crate::decode::webcodecs::WebCodecsDecoder;
use crate::error::MediaError;
use crate::player::{Player, PlayerState};
use crate::types::{AudioMetadata, FrameId, GopProfile, VideoMetadata};

/// Decoded frames held ready for canvas present (mediabunny-style prefetch).
/// Keep small — each slot pins a `VideoFrame`; 16 + in-flight decode starved
/// Chromium's pool on DRYP play (`flush/idle` mid-GOP with `decodeQ≈12`).
pub const PRESENTATION_QUEUE_CAP: usize = 6;
/// Frames to decode before starting the media clock (rest filled by prefetch).
const PLAY_PRIME_FRAMES: usize = 2;
/// Start-of-clip frames prepared into `loop_prime` before EOF (one decoder).
const LOOP_PRIME_FRAMES: usize = 8;
/// Wall-clock lag above this → skip to nearest keyframe instead of exact frame.
const BIG_LAG_SECONDS: f64 = 0.5;
/// Stop prefetching this many ms before the present deadline.
const PREFETCH_DEADLINE_SLACK_MS: f64 = 3.0;
/// Play Exact WebP trail after present (gated). Set `false` only for A/B jank checks.
const PLAY_EXACT_INGEST: bool = true;
/// Densify WebP jobs waiting while Worker is busy (blit already synced).
const WEBP_PENDING_CAP: usize = 4;

type PlayWebpOnMessage = Closure<dyn FnMut(MessageEvent)>;

struct PendingWebpJob {
    frame: FrameId,
    epoch: u64,
    width: u32,
    height: u32,
    replace: bool,
    bitmap: web_sys::ImageBitmap,
}

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
    presentation: RefCell<VecDeque<QueuedFrame>>,
    /// Exact scrub store (WebP chunks) — densify / seek / step residency.
    scrub_chunks: RefCell<ScrubChunkStore>,
    /// Live blit LRU for scrub present (`createImageBitmap`).
    scrub_blit: RefCell<ScrubBlitLru>,
    /// Reusable canvas for VideoFrame → WebP encode.
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
    /// Play Exact ingest counter (every-Nth from DPR). Reset on [`WebPlayer::play`].
    play_exact_presented: Cell<u64>,
    /// JS Worker for Play Exact WebP (`convertToBlob`); densify stays sync.
    play_webp_worker: RefCell<Option<Worker>>,
    play_webp_worker_url: RefCell<Option<String>>,
    play_webp_onmessage: RefCell<Option<PlayWebpOnMessage>>,
    play_webp_busy: Cell<bool>,
    play_webp_epoch: Cell<u64>,
    play_webp_failed: Cell<bool>,
    play_webp_next_id: Cell<u64>,
    play_webp_pending: RefCell<VecDeque<PendingWebpJob>>,
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
    /// Exact frame in the scrub WebP chunk store.
    Exact,
    /// In the play presentation / ahead queue (live `VideoFrame`).
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
            scrub_chunks: RefCell::new(ScrubChunkStore::new(max_frames)),
            scrub_blit: RefCell::new(ScrubBlitLru::new(SCRUB_BLIT_LRU)),
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
            play_exact_presented: Cell::new(0),
            play_webp_worker: RefCell::new(None),
            play_webp_worker_url: RefCell::new(None),
            play_webp_onmessage: RefCell::new(None),
            play_webp_busy: Cell::new(false),
            play_webp_epoch: Cell::new(0),
            play_webp_failed: Cell::new(false),
            play_webp_next_id: Cell::new(1),
            play_webp_pending: RefCell::new(VecDeque::new()),
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
        self.stop_media_clock()
    }

    pub fn state(&self) -> PlayerState {
        self.shared.player.borrow().state()
    }

    pub fn canvas(&self) -> &HtmlCanvasElement {
        &self.shared.canvas
    }

    /// Recompute canvas buffer from current CSS layout × DPR (call on container resize).
    ///
    /// Scrub WebP chunks match the playback buffer size — wipe Exact trail + blit
    /// LRU when the buffer changes so Pause→seek is not soft from an old encode.
    pub fn relayout(&self) -> Result<(), MediaError> {
        let meta = self.metadata();
        let before = (self.shared.canvas.width(), self.shared.canvas.height());
        configure_canvas_for_video(&self.shared.canvas, meta.width, meta.height)?;
        let after = (self.shared.canvas.width(), self.shared.canvas.height());
        if before != after {
            self.invalidate_scrub_store_for_resize();
        }
        Ok(())
    }

    /// Drop viewport-sized scrub residency after layout change (densify rebuilds).
    fn invalidate_scrub_store_for_resize(&self) {
        self.abort_scrub_fill(true);
        if let Ok(mut cache) = self.shared.scrub_chunks.try_borrow_mut() {
            cache.clear();
        }
        if let Ok(mut blit) = self.shared.scrub_blit.try_borrow_mut() {
            blit.clear();
        }
    }

    pub fn is_playing(&self) -> bool {
        self.state().playing
    }

    pub fn play(&self) {
        self.shared.fill_mode.set(CacheFillMode::Play);
        // Cancel densify hard — do **not** rearm `scrub_pending` (Play must not
        // keep filling gaps). Leave Exact scrub-store residency alone — eviction
        // is budget / farthest-from-aim only, never wipe on Play.
        self.cancel_ops();
        self.abort_scrub_fill(false);
        self.clear_scrub_direction();
        self.shared.player.borrow_mut().play();
        self.shared.frames_skipped.set(0);
        self.shared.last_behind_ms.set(None);
        self.shared.play_exact_presented.set(0);
        bump_play_webp_epoch(&self.shared);
    }

    pub fn pause(&self) {
        // Leaving play: cancel decode + scrub fill epoch (pending latch restarts fill).
        // Already paused: do not bump scrub_epoch (UI steps call pause every time).
        let was_playing = self.shared.player.borrow().state().playing;
        if was_playing {
            self.cancel_ops_and_scrub_fill();
            let _ = self.stop_media_clock();
            bump_play_webp_epoch(&self.shared);
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
                    || matches!(
                        shared.player.borrow().metadata().gop_profile,
                        GopProfile::AllIntra
                    )
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
        bump_play_webp_epoch(&self.shared);
        // Drop in-flight main-decoder cascade from the preceding drag sample so
        // settle does not inherit a wedged `decodeQueueSize`.
        self.invalidate_decoder();
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
        let (presentation_len, presentation_from, presentation_to, ahead) = {
            let presentation = self.shared.presentation.borrow();
            (
                presentation.len(),
                presentation.front().map(|q| q.frame),
                presentation.back().map(|q| q.frame),
                presentation.iter().map(|q| q.frame).collect::<Vec<_>>(),
            )
        };
        let (cache_chunks, cache_frames, cache_cap, cache_no_gaps, ready_frames, win_from, win_to) = {
            let cache = self.shared.scrub_chunks.borrow();
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
        // WebP store is scrub-drag only; never blit it for seek/step.
        self.abort_scrub_fill(true);
        self.shared.hold_densify_for_exact.set(true);
        self.shared.restarting.set(true);
        bump_play_webp_epoch(&self.shared);
        let result = async {
            self.shared.player.borrow_mut().pause();
            self.clear_loop_prime();
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

    /// Async timeline scrub (mediabunny `seekToKey` then `seekTo` under one epoch).
    ///
    /// - **Drag** (`settle=false`): densify while moving (warm cache under the finger);
    ///   preview + best-effort exact may race the scrub decoder. Gap pick follows
    ///   scrub direction (predictive densify).
    /// - **Settle** (`settle=true`, pointer up): abort densify, then one key→exact
    ///   cascade (`goto_frame_with_interim`) so canvas gets decode `VideoFrame`
    ///   (not store WebP — store is low-q for drag). Interim preview may blit
    ///   cache; pruned frames fill dense without blit.
    pub async fn scrub_to(&self, frame: FrameId, settle: bool) -> Result<(), MediaError> {
        let epoch = self.begin_op();
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
                self.goto_frame_with_interim(frame, epoch).await
            } else {
                if self.present_from_cache(frame).await? {
                    self.shared.player.borrow_mut().commit_frame(frame)?;
                    return Ok(());
                }

                // Drag: may decode key/neighbor for interim paint.
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
        // Reuse prefetch when present — live VideoFrame, not WebP store.
        {
            let mut presentation = self.shared.presentation.borrow_mut();
            if presentation.front().is_some_and(|f| f.frame == next) {
                let queued = presentation.pop_front().expect("front checked");
                drop(presentation);
                self.present_and_cache(queued.frame, queued.video)?;
                self.shared.player.borrow_mut().commit_frame(queued.frame)?;
                self.enter_scrub_cache_pipeline(queued.frame);
                self.snap_media_clock_to_playhead();
                return Ok(());
            }
        }
        // Always exact VF (mediabunny CanvasSink path). WebP blit is scrub-drag only.
        let epoch = self.begin_op();
        self.abort_scrub_fill(true);
        self.shared.hold_densify_for_exact.set(true);
        self.aim_scrub_cache(next);
        let result = self.goto_frame(next, epoch).await;
        self.shared.hold_densify_for_exact.set(false);
        if !self.is_current(epoch) {
            return Err(MediaError::Cancelled);
        }
        result?;
        self.snap_media_clock_to_playhead();
        self.arm_scrub_densify_after_exact(self.state().frame);
        Ok(())
    }

    pub async fn previous_frame(&self) -> Result<(), MediaError> {
        self.shared.player.borrow_mut().pause();
        let cur = self.state().frame;
        if cur == 0 {
            return Err(MediaError::FrameOutOfRange(0, self.state().frame_count));
        }
        let prev = cur - 1;
        // Always exact VF — do not present WebP store (scrub-drag only).
        let epoch = self.begin_op();
        self.abort_scrub_fill(true);
        self.shared.hold_densify_for_exact.set(true);
        self.aim_scrub_cache(prev);
        self.invalidate_decoder();
        let result = self.goto_frame(prev, epoch).await;
        self.shared.hold_densify_for_exact.set(false);
        if !self.is_current(epoch) {
            return Err(MediaError::Cancelled);
        }
        result?;
        self.snap_media_clock_to_playhead();
        self.arm_scrub_densify_after_exact(self.state().frame);
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
        // Scrub store is WebP-only — play re-decodes into the presentation queue.
        // Do **not** trim scrub residency on Play — eviction is
        // budget / farthest-from-aim only (Pause densify paid for that trail).
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

    /// Park presentation VFs on pause/scrub: encode **current** only, drop the rest.
    ///
    /// Sync WebP of the whole queue (up to `PRESENTATION_QUEUE_CAP` full frames)
    /// made Pause feel stuck. Densify refills Exact; one encode keeps Pause→Seek.
    fn park_presentation_into_cache(&self) {
        let items: Vec<(FrameId, OwnedVideoFrame)> = {
            let mut presentation = self.shared.presentation.borrow_mut();
            presentation.drain(..).map(|q| (q.frame, q.video)).collect()
        };
        let current = self.state().frame;
        for (frame, video) in items {
            if frame == current {
                self.ingest_scrub_frame(frame, video, true);
            } else {
                drop(video);
            }
        }
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

    /// Encode `video` into the scrub chunk store and drop the `VideoFrame`.
    fn ingest_scrub_frame(&self, frame: FrameId, video: OwnedVideoFrame, replace: bool) {
        ingest_scrub_frame_shared(&self.shared, frame, video, replace);
    }

    /// Play Exact trail ingest with cost gates (densify / scrub path unchanged).
    ///
    /// Prefers JS Worker `convertToBlob` (off main). Gates: every-Nth from DPR,
    /// skip when behind / no spare time, encode at CSS size. VF always dropped.
    fn ingest_play_exact_frame(&self, frame: FrameId, video: OwnedVideoFrame) {
        let n = self.shared.play_exact_presented.get();
        self.shared.play_exact_presented.set(n.saturating_add(1));

        let dpr = device_pixel_ratio();
        let stride = dpr.round().max(1.0) as u64;
        if !n.is_multiple_of(stride) {
            drop(video);
            return;
        }

        let media_time = self.media_time();
        if let Ok(start) = self.frame_timestamp_seconds(frame) {
            let lag_ms = ((media_time - start) * 1000.0).max(0.0);
            if lag_ms > 1.0 {
                drop(video);
                return;
            }
        }
        if let Ok(next_start) = self.frame_timestamp_seconds(frame.saturating_add(1)) {
            let ms_until = (next_start - media_time) * 1000.0;
            if ms_until < PREFETCH_DEADLINE_SLACK_MS {
                drop(video);
                return;
            }
        }

        let target_w = ((self.shared.canvas.width() as f64) / dpr).round().max(1.0) as u32;
        let target_h = ((self.shared.canvas.height() as f64) / dpr)
            .round()
            .max(1.0) as u32;

        if SCRUB_SKIP_PLAY_WEBP_DATA_URL {
            let _ =
                ingest_scrub_frame_at(&self.shared, frame, video, true, target_w, target_h, true);
            return;
        }

        if try_ingest_play_exact_via_worker(&self.shared, frame, &video, target_w, target_h) {
            drop(video);
            return;
        }

        let _ = ingest_scrub_frame_at(&self.shared, frame, video, true, target_w, target_h, false);
    }

    /// Draw a scrub-store hit via ImageBitmap LRU (`createImageBitmap` on miss).
    ///
    /// Pending Worker placeholders wait briefly for real bytes — do **not** treat
    /// them as hard misses (that forced key→N on densified Next/Prev).
    /// Decode failures return `Ok(false)` so callers fall through to key→N decode
    /// (corrupt chunks must not kill scrub).
    async fn present_from_cache(&self, frame: FrameId) -> Result<bool, MediaError> {
        {
            let Ok(mut blit) = self.shared.scrub_blit.try_borrow_mut() else {
                return Ok(false);
            };
            if let Some(bitmap) = blit.get(frame) {
                draw_image_bitmap(&self.shared.canvas, &self.shared.ctx, bitmap)?;
                return Ok(true);
            }
        }

        let mut chunk = {
            let Ok(store) = self.shared.scrub_chunks.try_borrow() else {
                return Ok(false);
            };
            store.get(frame).cloned()
        };
        let Some(first) = chunk.take() else {
            return Ok(false);
        };
        let mut chunk = first;

        if crate::decode::scrub_encode::is_pending_webp_chunk(&chunk) {
            for _ in 0..90 {
                yield_event_loop().await;
                let again = {
                    let Ok(store) = self.shared.scrub_chunks.try_borrow() else {
                        return Ok(false);
                    };
                    store.get(frame).cloned()
                };
                let Some(again) = again else {
                    return Ok(false);
                };
                if !crate::decode::scrub_encode::is_pending_webp_chunk(&again) {
                    chunk = again;
                    break;
                }
            }
            if crate::decode::scrub_encode::is_pending_webp_chunk(&chunk) {
                // Still encoding — resident for gaps, not a decode miss.
                return Ok(false);
            }
        }

        let bitmap = match image_bitmap_from_chunk(&chunk).await {
            Ok(b) => b,
            Err(e) => {
                web_sys::console::warn_1(
                    &format!("webmedia: scrub blit miss frame={frame} ({e})").into(),
                );
                invalidate_scrub_chunk(&self.shared, frame);
                return Ok(false);
            }
        };
        draw_image_bitmap(&self.shared.canvas, &self.shared.ctx, &bitmap)?;
        if let Ok(mut blit) = self.shared.scrub_blit.try_borrow_mut() {
            blit.set_playhead(self.state().frame);
            blit.put(frame, bitmap);
        } else {
            bitmap.close();
        }
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

    /// Exact seek that salvages pruned early frames into the scrub WebP store (no canvas blit).
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

    /// Draw and remember a visited frame in the scrub WebP store.
    ///
    /// Encodes after draw then drops the `VideoFrame` — Exact residency is compressed
    /// RAM, not Chromium's pool. Replace-on-hit so Pause→Seek/Prev/Next match canvas.
    fn present_and_cache(&self, frame: FrameId, video: OwnedVideoFrame) -> Result<(), MediaError> {
        let result =
            draw_frame_with_context(&self.shared.canvas, &self.shared.ctx, video.as_web_sys());
        self.ingest_scrub_frame(frame, video, true);
        result
    }

    /// Play present: draw + optional Exact ingest (WebP), then drop the live
    /// `VideoFrame` (tiny presentation pool).
    ///
    /// Presented-only trail — not densify. Do **not** call
    /// [`Self::schedule_scrub_cache_fill`] / scrub decoder here. Eviction remains
    /// budget / farthest-from-aim only (no Play trim wipe).
    ///
    /// [`PLAY_EXACT_INGEST`] gates the trail (A/B: set `false` to prove fullscreen
    /// jank is sync WebP encode).
    fn present_play_frame(&self, frame: FrameId, video: OwnedVideoFrame) -> Result<(), MediaError> {
        let result =
            draw_frame_with_context(&self.shared.canvas, &self.shared.ctx, video.as_web_sys());
        if PLAY_EXACT_INGEST {
            self.ingest_play_exact_frame(frame, video);
        } else {
            drop(video);
        }
        result
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

    /// Exact frame via decode (scrub store is WebP — no VideoFrame round-trip).
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
}

impl Drop for WebPlayer {
    fn drop(&mut self) {
        self.clear_loop_prime();
        self.clear_presentation();
        self.clear_scrub_store();
        terminate_play_webp_worker(&self.shared);
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
    let max_frames = meta.scrub_cache_frame_budget();
    shared.scrub_chunks.borrow_mut().set_max_frames(max_frames);
}

fn bump_play_webp_epoch(shared: &Shared) {
    shared
        .play_webp_epoch
        .set(shared.play_webp_epoch.get().wrapping_add(1));
    shared.play_webp_busy.set(false);
    clear_webp_pending(shared);
}

fn clear_webp_pending(shared: &Shared) {
    let Ok(mut q) = shared.play_webp_pending.try_borrow_mut() else {
        return;
    };
    while let Some(job) = q.pop_front() {
        job.bitmap.close();
    }
}

fn terminate_play_webp_worker(shared: &Shared) {
    bump_play_webp_epoch(shared);
    if let Ok(mut w) = shared.play_webp_worker.try_borrow_mut()
        && let Some(worker) = w.take()
    {
        worker.terminate();
    }
    if let Ok(mut url) = shared.play_webp_worker_url.try_borrow_mut()
        && let Some(u) = url.take()
    {
        revoke_worker_url(&u);
    }
    let _ = shared.play_webp_onmessage.try_borrow_mut().map(|mut c| {
        c.take();
    });
}

/// Post or queue a WebP job after blit bitmap is already in the LRU.
/// `allow_queue`: densify may wait; Play skips when busy.
/// Returns `true` if Worker path handled encode (or queued / skipped busy).
fn post_or_queue_webp(
    shared: &Rc<Shared>,
    frame: FrameId,
    width: u32,
    height: u32,
    replace: bool,
    worker_bitmap: web_sys::ImageBitmap,
    allow_queue: bool,
) -> bool {
    if shared.play_webp_failed.get() {
        worker_bitmap.close();
        return false;
    }
    if !ensure_play_webp_worker(shared) {
        worker_bitmap.close();
        return false;
    }

    let epoch = shared.play_webp_epoch.get();
    if shared.play_webp_busy.get() {
        if !allow_queue {
            worker_bitmap.close();
            return true;
        }
        let Ok(mut q) = shared.play_webp_pending.try_borrow_mut() else {
            worker_bitmap.close();
            return true;
        };
        if q.len() >= WEBP_PENDING_CAP {
            worker_bitmap.close();
            return false;
        }
        q.push_back(PendingWebpJob {
            frame,
            epoch,
            width,
            height,
            replace,
            bitmap: worker_bitmap,
        });
        return true;
    }

    let id = shared.play_webp_next_id.get();
    shared.play_webp_next_id.set(id.wrapping_add(1));
    shared.play_webp_busy.set(true);
    let posted = {
        let Ok(borrow) = shared.play_webp_worker.try_borrow() else {
            shared.play_webp_busy.set(false);
            worker_bitmap.close();
            return false;
        };
        let Some(worker) = borrow.as_ref() else {
            shared.play_webp_busy.set(false);
            worker_bitmap.close();
            return false;
        };
        post_play_webp_job(PlayWebpJob {
            worker,
            id,
            frame,
            epoch,
            width,
            height,
            replace,
            bitmap: worker_bitmap,
        })
    };
    match posted {
        Ok(()) => true,
        Err(e) => {
            shared.play_webp_busy.set(false);
            web_sys::console::warn_1(
                &format!("webmedia: WebP worker post failed ({e}); sync encode").into(),
            );
            shared.play_webp_failed.set(true);
            false
        }
    }
}

fn pump_webp_pending(shared: &Rc<Shared>) {
    if shared.play_webp_busy.get() || shared.play_webp_failed.get() {
        return;
    }
    let job = {
        let Ok(mut q) = shared.play_webp_pending.try_borrow_mut() else {
            return;
        };
        loop {
            let Some(job) = q.pop_front() else {
                return;
            };
            if job.epoch != shared.play_webp_epoch.get() {
                job.bitmap.close();
                continue;
            }
            break job;
        }
    };
    let id = shared.play_webp_next_id.get();
    shared.play_webp_next_id.set(id.wrapping_add(1));
    shared.play_webp_busy.set(true);
    let posted = {
        let Ok(borrow) = shared.play_webp_worker.try_borrow() else {
            shared.play_webp_busy.set(false);
            job.bitmap.close();
            return;
        };
        let Some(worker) = borrow.as_ref() else {
            shared.play_webp_busy.set(false);
            job.bitmap.close();
            return;
        };
        post_play_webp_job(PlayWebpJob {
            worker,
            id,
            frame: job.frame,
            epoch: job.epoch,
            width: job.width,
            height: job.height,
            replace: job.replace,
            bitmap: job.bitmap,
        })
    };
    if let Err(e) = posted {
        shared.play_webp_busy.set(false);
        web_sys::console::warn_1(
            &format!("webmedia: WebP worker pending post failed ({e})").into(),
        );
        shared.play_webp_failed.set(true);
        clear_webp_pending(shared);
    }
}

/// `true` = handled (posted, queued, or skipped while busy). `false` = sync fallback.
fn try_ingest_play_exact_via_worker(
    shared: &Rc<Shared>,
    frame: FrameId,
    video: &OwnedVideoFrame,
    target_w: u32,
    target_h: u32,
) -> bool {
    if shared.play_webp_failed.get() {
        return false;
    }
    let pair = match shared
        .encode_canvas
        .with_encoder(|enc| enc.frame_to_bitmap_pair(video.as_web_sys(), target_w, target_h))
    {
        Ok(p) => p,
        Err(e) => {
            web_sys::console::warn_1(
                &format!("webmedia: Play WebP transfer failed ({e}); sync encode").into(),
            );
            return false;
        }
    };
    let (blit_bmp, worker_bmp, width, height) = pair;
    if let Ok(mut blit) = shared.scrub_blit.try_borrow_mut() {
        blit.set_playhead(shared.scrub_chunks.borrow().playhead());
        blit.put(frame, blit_bmp);
    } else {
        blit_bmp.close();
    }
    post_or_queue_webp(shared, frame, width, height, true, worker_bmp, false)
}

/// Densify / scrub ingest via Worker: full-res blit + CSS-sized WebP job.
/// `true` if handled (gap closed on main; bytes may arrive later).
fn try_ingest_scrub_via_worker(
    shared: &Rc<Shared>,
    frame: FrameId,
    video: &OwnedVideoFrame,
    target_w: u32,
    target_h: u32,
    replace: bool,
) -> bool {
    if shared.play_webp_failed.get() {
        return false;
    }
    let dpr = device_pixel_ratio();
    let css_w = ((target_w as f64) / dpr).round().max(1.0) as u32;
    let css_h = ((target_h as f64) / dpr).round().max(1.0) as u32;

    let blit_bmp = match shared
        .encode_canvas
        .with_encoder(|enc| enc.frame_to_bitmap(video.as_web_sys(), target_w, target_h))
    {
        Ok((b, _, _)) => b,
        Err(_) => return false,
    };
    let (worker_bmp, width, height) = match shared
        .encode_canvas
        .with_encoder(|enc| enc.frame_to_bitmap(video.as_web_sys(), css_w, css_h))
    {
        Ok(p) => p,
        Err(_) => {
            blit_bmp.close();
            return false;
        }
    };

    // Close the gap immediately so densify does not re-decode the same frames.
    let placeholder = ScrubChunk {
        bytes: vec![0],
        width,
        height,
        mime: PENDING_WEBP_MIME.into(),
    };
    {
        let Ok(mut store) = shared.scrub_chunks.try_borrow_mut() else {
            blit_bmp.close();
            worker_bmp.close();
            return false;
        };
        if replace {
            store.put_chunk(frame, placeholder);
        } else if !store.insert_chunk(frame, placeholder) {
            blit_bmp.close();
            worker_bmp.close();
            return false;
        }
    }

    if let Ok(mut blit) = shared.scrub_blit.try_borrow_mut() {
        blit.set_playhead(shared.scrub_chunks.borrow().playhead());
        blit.put(frame, blit_bmp);
    } else {
        blit_bmp.close();
        worker_bmp.close();
        let _ = shared
            .scrub_chunks
            .try_borrow_mut()
            .map(|mut s| s.remove(frame));
        return false;
    }
    // Always replace when bytes arrive (placeholder → real).
    if !post_or_queue_webp(shared, frame, width, height, true, worker_bmp, true) {
        let _ = shared
            .scrub_chunks
            .try_borrow_mut()
            .map(|mut s| s.remove(frame));
        return false;
    }
    true
}

fn ensure_play_webp_worker(shared: &Rc<Shared>) -> bool {
    if shared.play_webp_worker.borrow().is_some() {
        return true;
    }
    let shared_cb = Rc::clone(shared);
    let onmessage = Closure::wrap(Box::new(move |ev: MessageEvent| {
        on_play_webp_message(&shared_cb, &ev);
    }) as Box<dyn FnMut(MessageEvent)>);

    match spawn_play_webp_worker(&onmessage) {
        Ok((worker, url)) => {
            *shared.play_webp_worker.borrow_mut() = Some(worker);
            *shared.play_webp_worker_url.borrow_mut() = Some(url);
            *shared.play_webp_onmessage.borrow_mut() = Some(onmessage);
            true
        }
        Err(e) => {
            web_sys::console::warn_1(
                &format!("webmedia: WebP worker spawn failed ({e}); sync encode").into(),
            );
            shared.play_webp_failed.set(true);
            false
        }
    }
}

fn on_play_webp_message(shared: &Rc<Shared>, ev: &MessageEvent) {
    shared.play_webp_busy.set(false);
    match parse_play_webp_reply(ev) {
        Ok(Some(reply)) => {
            if reply.epoch == shared.play_webp_epoch.get()
                && let Ok(mut store) = shared.scrub_chunks.try_borrow_mut()
            {
                if reply.replace {
                    store.put_chunk(reply.frame, reply.chunk);
                } else {
                    let _ = store.insert_chunk(reply.frame, reply.chunk);
                }
            }
            pump_webp_pending(shared);
        }
        Ok(None) => {
            pump_webp_pending(shared);
        }
        Err(e) => {
            web_sys::console::warn_1(&format!("webmedia: WebP worker error ({e})").into());
            pump_webp_pending(shared);
        }
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
    if try_ingest_scrub_via_worker(shared, frame, &video, target_w, target_h, replace) {
        drop(video);
        return true;
    }
    ingest_scrub_frame_at(shared, frame, video, replace, target_w, target_h, false)
}

fn ingest_scrub_frame_at(
    shared: &Rc<Shared>,
    frame: FrameId,
    video: OwnedVideoFrame,
    replace: bool,
    target_w: u32,
    target_h: u32,
    skip_data_url: bool,
) -> bool {
    let encoded = shared.encode_canvas.with_encoder(|enc| {
        enc.encode_webp_with_blit(
            video.as_web_sys(),
            target_w.max(1),
            target_h.max(1),
            skip_data_url,
        )
    });
    drop(video);
    let Ok(encoded) = encoded else {
        return false;
    };
    let crate::decode::scrub_encode::EncodedScrubFrame { chunk, bitmap } = encoded;
    {
        let Ok(mut store) = shared.scrub_chunks.try_borrow_mut() else {
            bitmap.close();
            return false;
        };
        if replace {
            store.put_chunk(frame, chunk);
        } else if !store.insert_chunk(frame, chunk) {
            bitmap.close();
            return false;
        }
    }
    if let Ok(mut blit) = shared.scrub_blit.try_borrow_mut() {
        blit.set_playhead(shared.scrub_chunks.borrow().playhead());
        blit.put(frame, bitmap);
    } else {
        bitmap.close();
    }
    true
}

fn invalidate_scrub_chunk(shared: &Shared, frame: FrameId) {
    if let Ok(mut blit) = shared.scrub_blit.try_borrow_mut() {
        blit.invalidate(frame);
    }
    if let Ok(mut store) = shared.scrub_chunks.try_borrow_mut() {
        let _ = store.remove(frame);
    }
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
    let cache = shared.scrub_chunks.borrow();
    if cache.gaps_saturated(playhead, last, dir) || !cache.gap_fill_armed(playhead) {
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
        // Only skip when this GOP is fully resident — do not bail just because
        // `target` exists (holes elsewhere in the GOP must still densify).
        // If the gap `target` is still missing, the complete check is stale /
        // inconsistent — fall through and decode.
        let cache = shared.scrub_chunks.borrow();
        if !ids.is_empty() && cache.gop_complete(&ids) && cache.has_frame(target) {
            // Gap already closed — count as progress so idle_passes does not trip.
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

    // Stream into scrub WebP store as each remapped frame emerges so the ready
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
