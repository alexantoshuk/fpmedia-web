//! Browser video player façade: demux + WebCodecs decode + canvas (+ optional audio).
//!
//! Decode plumbing lives in [`crate::decode`]; this module owns play/scrub/seek orchestration.
//! Present and decode share the player via interior mutability so an rAF present loop
//! never waits on `prefetch_one` (mediabunny-style split).

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::rc::Rc;

use wasm_bindgen::JsCast;
use wasm_bindgen::closure::Closure;
use web_sys::{CanvasRenderingContext2d, HtmlCanvasElement};

use crate::MediaClock;
use crate::audio::AudioOutput;
use crate::decode::canvas::{canvas_2d, configure_canvas_for_video, draw_frame_with_context};
use crate::decode::frame_cache::{ScrubBlitLru, ScrubChunkStore};
use crate::decode::owned_video_frame::OwnedVideoFrame;
use crate::decode::scrub_blit::BlitCanvasSlot;
#[cfg(feature = "thumbs")]
use crate::decode::thumb_preview::ThumbPreview;
use crate::decode::webcodecs::WebCodecsDecoder;
use crate::decoder_slot::{
    DecoderSlotCmd, DecoderSlotEvent, DecoderSlotModel, reduce as decoder_slot_reduce,
};
use crate::engine_mode::{EngineEvent, EngineModel, reduce as engine_reduce};
use crate::error::MediaError;
use crate::player::{Player, PlayerState};
use crate::types::{
    AudioMetadata, FrameId, GopProfile, ThumbPolicy, VideoMetadata, coalesce_frame_runs,
};
use crate::workload::{
    FRAGMENT_BACKFILL_IDLE_AHEAD, WorkloadHints, allows_bg_sync_verify, classify,
    fragment_backfill_should_idle_yield,
};

mod play;
mod scrub;
mod seek;

use scrub::{
    apply_clip_budgets, ingest_scrub_bitmap_only, ingest_scrub_frame_shared, next_gap_target,
    scrub_cache_fill_task,
};

/// Live VF ahead of the playhead (play prefetch + Next hits).
/// Keep small on Hd — each slot pins a `VideoFrame`; 16 + in-flight decode starved
/// Chromium's pool on long-GOP play (`flush/idle` mid-GOP with `decodeQ≈12`).
/// Pair with [`STEP_BEHIND_CAP`] (ring ≈ 1 displayed + 8 behind + 4 ahead).
/// Hd default = [`crate::PLAY_PRESENTATION_CAP_HD`]; Uhd uses [`PlayPolicy`].
pub const PRESENTATION_QUEUE_CAP: usize = crate::PLAY_PRESENTATION_CAP_HD;
/// Ahead depth while shuttling forward (`rate >= 2`) — keep decode light.
pub const SHUTTLE_PRESENTATION_QUEUE_CAP: usize = 2;
/// Live VF older than the playhead for Prev without key→N (step/scrub ring).
/// mediabunny reverse always restarts from key unless samples are retained —
/// this is the SampleCursor-lite behind window (`media-sink.ts` flush on rewind).
/// **Play must not keep this ring** — 1+8+4 + held≈8 pins ~21 VideoFrames and
/// starves Chromium's pool (underrun hold ≈0.77× realtime on long-GOP). Forward play
/// uses cap 0; Pause/scrub restores [`STEP_BEHIND_CAP`].
pub const STEP_BEHIND_CAP: usize = 8;
/// Behind ring while play mode — drop aged VF immediately.
const STEP_BEHIND_CAP_PLAY: usize = 0;
/// Extra in-flight / held VF beyond displayed + presentation queue when sizing
/// Exact trim on Play (YUV↔RGBA heuristic headroom).
const PLAY_EXACT_VRAM_VF_SLACK: usize = 4;
/// Frames to decode before starting the media clock (rest filled by prefetch).
const PLAY_PRIME_FRAMES: usize = 2;
/// Start-of-clip frames prepared into `loop_prime` before EOF (one decoder).
/// Hd default alias — live value from [`PlayPolicy::loop_prime_frames`].
#[allow(dead_code)]
const LOOP_PRIME_FRAMES: usize = crate::PLAY_LOOP_PRIME_FRAMES_HD;
/// Wall-time budget for `fill_loop_prime` / near-EOF cap (Hd alias).
/// Live value from [`PlayPolicy::loop_prime_budget_secs`].
#[allow(dead_code)]
const LOOP_PRIME_BUDGET_SECS: f64 = crate::PLAY_EOF_BUDGET_SECS_HD;
/// Ceiling on near-EOF presentation growth (live VF VRAM).
const LOOP_NEAR_END_PRESENTATION_CAP_MAX: usize = 64;
/// Wall-clock lag above this → skip to nearest keyframe instead of exact frame.
const BIG_LAG_SECONDS: f64 = 0.5;
/// Stop prefetching this many ms before the present deadline.
const PREFETCH_DEADLINE_SLACK_MS: f64 = 3.0;

struct QueuedFrame {
    frame: FrameId,
    video: OwnedVideoFrame,
}

/// Single playhead cache — fill policy lives in [`EngineModel`].
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
    encode_canvas: BlitCanvasSlot,
    /// Hover / filmstrip sink (`ThumbPolicy::On` → lazy create). Feature `thumbs`.
    /// `Rc` so sparse fill can hold the sink across `.await` without `RefCell` borrow.
    #[cfg(feature = "thumbs")]
    thumb: RefCell<Option<Rc<ThumbPreview>>>,
    #[cfg(feature = "thumbs")]
    thumb_policy: Cell<ThumbPolicy>,
    /// Second decoder for background gap fill (does not steal main playback).
    scrub_decoder: RefCell<Option<WebCodecsDecoder>>,
    /// Densify create failed permanently this clip (e.g. HEVC HW-only — no SW
    /// second decoder). Stops retry storms that would call `prefer-hardware`.
    scrub_densify_blocked: Cell<bool>,
    scrub_epoch: Cell<u64>,
    scrub_busy: Cell<bool>,
    /// Set while a fill is running so `schedule_scrub_cache_fill` can request another
    /// pass (avoids dropping work when pause/scrub bumps epoch mid-fill).
    scrub_pending: Cell<bool>,
    /// Play vs scrub densify / exact hold + soft-hold audio.
    engine: Cell<EngineModel>,
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
    preparing_loop: Cell<bool>,
    /// Bumped to cancel in-flight decode (mediabunny dispose / asyncId).
    op_epoch: Cell<u64>,
    /// Exclusive main-decoder checkout + reset-pending ([`DecoderSlotModel`]).
    decoder_slot: Cell<DecoderSlotModel>,
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
    /// File-byte fetch coverage (demo scrubber gray strip).
    fetch_map: std::sync::Arc<crate::FetchMap>,
    file_size: u64,
}

impl Shared {
    fn workload_hints(&self) -> WorkloadHints {
        WorkloadHints::new(
            self.engine.get().mode,
            self.restarting.get(),
            self.underrun_hold_since_ms.get().is_some(),
            self.scrub_busy.get(),
        )
    }
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
    /// Underrun clock-freeze edges since last [`VideoPlayer::play`].
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
    /// Bounded union of recent [`crate::CachedSource`] windows as file-size
    /// fractions (`0..1`) for the demo gray strip (not densify — green below).
    pub fetch_file_ranges: Vec<(f32, f32)>,
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
            fetch_file_ranges: Vec::new(),
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

/// Frame-accurate video player (WebCodecs → canvas); optional audio via [`crate::AudioOutput`].
/// Pure-audio hosts will use a separate `AudioPlayer` over the same audio modules.
pub struct VideoPlayer {
    shared: Rc<Shared>,
    /// `None` only while an async decode owns the decoder (must not re-enter).
    decoder: RefCell<Option<WebCodecsDecoder>>,
}

impl VideoPlayer {
    pub async fn open_mp4_bytes(
        bytes: Vec<u8>,
        canvas: HtmlCanvasElement,
    ) -> Result<Self, MediaError> {
        let opened = crate::open_mp4_media_bytes(bytes)?;
        let video = opened.video.ok_or(MediaError::NoVideoTrack)?;
        Self::from_tracks_with_fetch(video, opened.audio, canvas, opened.fetch_map).await
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
                defer_avc_sync: true,
            },
        )
        .await?;
        let video = opened.video.ok_or(MediaError::NoVideoTrack)?;
        Self::from_tracks_with_fetch(video, opened.audio, canvas, opened.fetch_map).await
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
        audio: Option<crate::audio::Mp4AudioTrack>,
        canvas: HtmlCanvasElement,
    ) -> Result<Self, MediaError> {
        Self::from_tracks_with_fetch(
            track,
            audio,
            canvas,
            std::sync::Arc::new(crate::FetchMap::new()),
        )
        .await
    }

    /// Like [`Self::from_tracks`], with a shared file-byte [`crate::FetchMap`].
    pub async fn from_tracks_with_fetch(
        track: crate::demux::Mp4VideoTrack,
        audio: Option<crate::audio::Mp4AudioTrack>,
        canvas: HtmlCanvasElement,
        fetch_map: std::sync::Arc<crate::FetchMap>,
    ) -> Result<Self, MediaError> {
        let file_size = track.source().size();
        let player = Player::from_track(track)?;
        Self::from_player(player, audio, canvas, fetch_map, file_size).await
    }

    async fn from_player(
        player: Player,
        audio_track: Option<crate::audio::Mp4AudioTrack>,
        canvas: HtmlCanvasElement,
        fetch_map: std::sync::Arc<crate::FetchMap>,
        file_size: u64,
    ) -> Result<Self, MediaError> {
        let meta = player.metadata();
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
            encode_canvas: BlitCanvasSlot::default(),
            #[cfg(feature = "thumbs")]
            thumb: RefCell::new(None),
            #[cfg(feature = "thumbs")]
            thumb_policy: Cell::new(ThumbPolicy::On),
            scrub_decoder: RefCell::new(None),
            scrub_densify_blocked: Cell::new(false),
            scrub_epoch: Cell::new(0),
            scrub_busy: Cell::new(false),
            scrub_pending: Cell::new(false),
            engine: Cell::new(EngineModel::new()),
            loop_prime: RefCell::new(None),
            canvas,
            ctx,
            frames_skipped: Cell::new(0),
            last_behind_ms: Cell::new(None),
            underrun_holds: Cell::new(0),
            underrun_hold_ms: Cell::new(0.0),
            underrun_hold_since_ms: Cell::new(None),
            restarting: Cell::new(false),
            preparing_loop: Cell::new(false),
            op_epoch: Cell::new(0),
            decoder_slot: Cell::new(DecoderSlotModel::new()),
            fill_stalled_aim: Cell::new(None),
            scrub_dir: Cell::new(0),
            prev_scrub_aim: Cell::new(None),
            audio: RefCell::new(audio),
            clock: RefCell::new(MediaClock::new()),
            wall_origin: Cell::new(None),
            fetch_map,
            file_size,
        });
        apply_clip_budgets(&shared);
        if let Some(a) = shared.audio.borrow().as_ref() {
            shared
                .player
                .borrow()
                .track_rc()
                .set_backfill_audio_sink(std::rc::Rc::downgrade(a));
        }
        Self::kick_deferred_sync_verify(&shared);
        Self::kick_fragment_backfill(&shared);
        // Do not seek here — first-frame decode can be slow; let the host show
        // metadata first and call `seek_frame(0)` with its own progress UI.
        Ok(Self {
            shared,
            decoder: RefCell::new(Some(decoder)),
        })
    }

    /// Background fMP4 moof backfill after warm open (mediabunny: `readFragment`).
    ///
    /// Chunked: each step extends the index so play past warm moofs does not wait
    /// for the entire mfra list (`ensure_frame_indexed` polls for the needed frame).
    fn kick_fragment_backfill(shared: &Rc<Shared>) {
        let track = shared.player.borrow().track_rc();
        if !track.begin_fragment_backfill_worker() {
            return;
        }
        // Race index ahead of a quick Play click (ready ≠ indexed lead).
        track.request_index_through(160);
        if let Some(a) = shared.audio.borrow().as_ref() {
            a.set_expect_more_samples(true);
        }
        web_sys::console::log_1(
            &format!(
                "webmedia: fragment backfill bg (indexed={}/{})",
                track.indexed_frame_count(),
                track.frame_count()
            )
            .into(),
        );
        let track = track.clone();
        let shared = Rc::clone(shared);
        wasm_bindgen_futures::spawn_local(async move {
            loop {
                match track.run_fragment_backfill_step().await {
                    Ok((more, audio)) => {
                        if let Some(fresh) = audio.as_ref()
                            && let Some(out) = shared.audio.borrow().as_ref()
                        {
                            out.adopt_fragment_samples(fresh);
                        }
                        if !more {
                            if let Some(out) = shared.audio.borrow().as_ref() {
                                out.set_expect_more_samples(false);
                            }
                            web_sys::console::log_1(
                                &format!(
                                    "webmedia: fragment backfill done ({} frames)",
                                    track.frame_count()
                                )
                                .into(),
                            );
                            break;
                        }
                        web_sys::console::log_1(
                            &format!(
                                "webmedia: fragment backfill chunk (indexed={}/{})",
                                track.indexed_frame_count(),
                                track.frame_count()
                            )
                            .into(),
                        );
                        // Keep index well ahead of the playhead; only idle-yield when
                        // paused/scrubbing with ≥240 frames of lead.
                        let mode = classify(shared.workload_hints());
                        let playhead = shared.player.borrow().state().frame;
                        let indexed = track.indexed_frame_count();
                        let ahead = indexed.saturating_sub(playhead);
                        if !fragment_backfill_should_idle_yield(
                            mode,
                            ahead,
                            track.fragment_backfill_urgent(),
                            FRAGMENT_BACKFILL_IDLE_AHEAD,
                        ) {
                            continue;
                        }
                        crate::util::yield_now().await;
                    }
                    Err(e) => {
                        if let Some(out) = shared.audio.borrow().as_ref() {
                            out.set_expect_more_samples(false);
                        }
                        web_sys::console::error_1(
                            &format!("webmedia: fragment backfill failed: {e}").into(),
                        );
                        break;
                    }
                }
            }
        });
    }

    /// Background bitstream sync verify (deferred open). mediabunny verifies on
    /// packet retrieve; bg walk finishes the table for scrub geometry without
    /// blocking settle (settle uses [`Mp4VideoTrack::ensure_key_verified`] only).
    fn kick_deferred_sync_verify(shared: &Rc<Shared>) {
        let track = shared.player.borrow().track_rc();
        if track.sync_verify_complete() {
            return;
        }
        let pending = track.sync_verify_pending();
        web_sys::console::log_1(
            &format!("webmedia: deferred sync verify bg ({pending} candidates)").into(),
        );
        let weak = Rc::downgrade(shared);
        wasm_bindgen_futures::spawn_local(async move {
            loop {
                let Some(shared) = weak.upgrade() else {
                    break;
                };
                // Yield while Play / Seek own the network, on underrun, or while
                // densify is busy (avoid GitHub/proxy 502 + sample-sized Range storms).
                // mediabunny: verifyKeyPackets on retrieve — no bg full-file walk
                // during playback (`WorkloadMode` / `allows_bg_sync_verify`).
                if !allows_bg_sync_verify(shared.workload_hints()) {
                    crate::util::yield_now().await;
                    continue;
                }
                let track = shared.player.borrow().track_rc();
                if track.sync_verify_complete() {
                    break;
                }
                match track.verify_next_pending().await {
                    Ok(true) => crate::util::yield_now().await,
                    Ok(false) => {
                        web_sys::console::log_1(&"webmedia: deferred sync verify done".into());
                        break;
                    }
                    Err(e) => {
                        web_sys::console::warn_1(
                            &format!("webmedia: deferred sync verify stopped ({e})").into(),
                        );
                        break;
                    }
                }
            }
        });
    }

    pub fn metadata(&self) -> VideoMetadata {
        self.shared.player.borrow().metadata()
    }

    pub fn audio_metadata(&self) -> Option<AudioMetadata> {
        self.shared.audio.borrow().as_ref().map(|a| a.metadata())
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

    pub fn playback_rate(&self) -> f64 {
        self.shared.clock.borrow().playback_rate()
    }

    /// True when current rate uses shuttle policy (`|rate| >= 2`).
    pub fn is_shuttle(&self) -> bool {
        self.shared.clock.borrow().is_shuttle()
    }

    /// Playback rate: forward `0.25…8`.
    /// Above [`crate::PLAYBACK_RATE_AUDIO_MAX`] → wall clock, audio off
    /// ([`crate::is_wall_clock_rate`]). Through 4× keep `AudioContext` +
    /// fixed-pitch WSOLA stretch (shuttle **present** policy still `≥2`).
    /// Live change snaps clock origins.
    ///
    /// On rate **decrease**, snap to the displayed/committed playhead — shuttle
    /// disables underrun hold so `media_time` can run ahead of decode; keeping
    /// that raced time makes J (8→4→2→1) keep skip-catching for seconds.
    /// `mediabunny: no equivalent` (no rate API).
    pub fn set_playback_rate(&self, rate: f64) {
        let rate = crate::normalize_playback_rate(rate);
        let wall = crate::is_wall_clock_rate(rate);
        let audio = self.shared.audio.borrow().as_ref().map(Rc::clone);
        let old_rate = self.shared.clock.borrow().playback_rate();
        let clock_t = self.media_time().max(0.0);
        let t = if rate < old_rate - 1e-9 {
            self.rate_decrease_snap_time(clock_t)
        } else {
            clock_t
        };
        let was_playing = self.shared.clock.borrow().is_playing()
            || audio.as_ref().is_some_and(|a| a.is_playing());

        if let Some(a) = audio {
            if wall {
                if a.is_playing() {
                    let _ = a.stop();
                }
                a.store_playback_rate(rate);
                self.shared.clock.borrow_mut().set_playback_rate(rate);
                if was_playing {
                    self.shared.clock.borrow_mut().start(t);
                    if let Some(now) = performance_now_ms() {
                        self.shared.wall_origin.set(Some(now / 1000.0));
                    }
                }
            } else {
                // Leave shuttle (wall) → review/1× audio while still playing.
                self.shared.clock.borrow_mut().set_playback_rate(rate);
                if was_playing && !a.is_playing() {
                    self.shared.clock.borrow_mut().start(t);
                    self.shared.wall_origin.set(None);
                    a.store_playback_rate(rate);
                    let audio = Rc::clone(&a);
                    let shared = Rc::clone(&self.shared);
                    let start_t = t;
                    wasm_bindgen_futures::spawn_local(async move {
                        // Host may have paused after rate snap (K: pause then 1×,
                        // or rate→pause race). Do not restart audio into a stop.
                        if !shared.player.borrow().state().playing
                            || crate::is_wall_clock_rate(shared.clock.borrow().playback_rate())
                        {
                            return;
                        }
                        if let Err(e) = audio.start_spawned(start_t).await {
                            web_sys::console::warn_1(
                                &format!("webmedia: audio after shuttle: {e}").into(),
                            );
                        }
                    });
                } else if a.is_playing() {
                    a.set_playback_rate(rate);
                    self.shared.clock.borrow_mut().start(t);
                } else {
                    a.store_playback_rate(rate);
                }
            }
        } else {
            let playing = self.shared.clock.borrow().is_playing();
            self.shared.clock.borrow_mut().set_playback_rate(rate);
            if playing {
                self.shared.clock.borrow_mut().start(t);
                if let Some(now) = performance_now_ms() {
                    self.shared.wall_origin.set(Some(now / 1000.0));
                }
            }
        }
    }

    /// Visual playhead time for rate-down snaps (canvas, else committed).
    fn rate_decrease_snap_time(&self, clock_t: f64) -> f64 {
        let head = self
            .shared
            .displayed
            .borrow()
            .as_ref()
            .and_then(|q| self.frame_timestamp_seconds(q.frame).ok())
            .unwrap_or_else(|| self.state().timestamp.seconds)
            .max(0.0);
        head.min(clock_t)
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
    /// Review / 1×–4× + audio track → decode/schedule pump (mediabunny `play` +
    /// `runAudioIterator`; non-1× adds WSOLA stretch). Above 4× / no audio
    /// → wall `performance`.
    pub async fn start_media_clock(&self, media_time_secs: f64) -> Result<(), MediaError> {
        self.finish_underrun_hold_timer();
        self.shared.clock.borrow_mut().start(media_time_secs);
        let audio = self.shared.audio.borrow().as_ref().map(Rc::clone);
        if crate::is_wall_clock_rate(self.playback_rate()) {
            if let Some(a) = audio {
                let _ = a.stop();
                a.store_playback_rate(self.playback_rate());
            }
            if let Some(now) = performance_now_ms() {
                self.shared.wall_origin.set(Some(now / 1000.0));
            }
            return Ok(());
        }
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
        let mut engine = self.shared.engine.get();
        engine.audio = crate::AudioResume::Cold;
        self.shared.engine.set(engine);
        t
    }

    /// Soft-silence for scrub-while-play — keep `AudioDecoder` configured.
    ///
    /// Next PlayPrime should [`Self::resume_media_clock`] (not cold
    /// [`Self::start_media_clock`]). Hard Pause / open still use
    /// [`Self::stop_media_clock`].
    /// `mediabunny: examples/media-player/media-player.ts` `pause`.
    pub fn hold_media_clock_for_scrub(&self) -> f64 {
        let t = if let Some(a) = self.shared.audio.borrow().as_ref() {
            a.hold_underrun()
        } else {
            self.media_time()
        };
        self.shared.wall_origin.set(None);
        self.shared.clock.borrow_mut().snap(t);
        let mut engine = self.shared.engine.get();
        engine.audio = crate::AudioResume::Warm;
        self.shared.engine.set(engine);
        t
    }

    /// Consume the scrub soft-hold latch (true → warm audio resume).
    pub fn take_audio_warm_resume(&self) -> bool {
        let mut engine = self.shared.engine.get();
        let warm = engine.take_warm_audio();
        self.shared.engine.set(engine);
        warm
    }

    /// True while soft-held after scrub-while-play (settle may seed play frames).
    pub(crate) fn audio_warm_resume(&self) -> bool {
        self.shared.engine.get().warm_audio()
    }

    /// Freeze the media clock while the presentation queue is empty.
    ///
    /// Prefer this over `skip_behind` when Chromium decode briefly lags the
    /// audio clock (long-GOP: skip spiral → `skipped≈242` then End before EOF).
    /// Keeps `playing` true; host resumes via [`Self::resume_media_clock`] once
    /// prefetch has frames again.
    ///
    /// Soft-holds audio ([`AudioOutput::hold_underrun`]) — silence without
    /// `AudioDecoder` reset — so resume is not a cold reconfigure gap.
    pub fn hold_media_clock_for_underrun(&self) -> f64 {
        self.note_underrun_hold_start();
        let t = if let Some(a) = self.shared.audio.borrow().as_ref() {
            a.hold_underrun()
        } else {
            self.media_time()
        };
        self.shared.wall_origin.set(None);
        self.shared.clock.borrow_mut().snap(t);
        t
    }

    /// Warm resume after [`Self::hold_media_clock_for_underrun`], scrub soft-hold,
    /// or loop seam.
    ///
    /// Reuses the configured `AudioDecoder` + primes the first buffers
    /// ([`AudioOutput::restart_warm`]). Fresh Play after hard pause still uses
    /// [`Self::start_media_clock`] (cold `begin`).
    pub async fn resume_media_clock(&self, media_time_secs: f64) -> Result<(), MediaError> {
        self.finish_underrun_hold_timer();
        self.shared.clock.borrow_mut().start(media_time_secs);
        let audio = self.shared.audio.borrow().as_ref().map(Rc::clone);
        if crate::is_wall_clock_rate(self.playback_rate()) {
            if let Some(a) = audio {
                let _ = a.stop();
                a.store_playback_rate(self.playback_rate());
            }
            if let Some(now) = performance_now_ms() {
                self.shared.wall_origin.set(Some(now / 1000.0));
            }
            return Ok(());
        }
        if let Some(a) = audio {
            self.shared.wall_origin.set(None);
            a.restart_warm(media_time_secs).await?;
        } else if let Some(now) = performance_now_ms() {
            self.shared.wall_origin.set(Some(now / 1000.0));
        }
        Ok(())
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
        let (next, _) = engine_reduce(self.shared.engine.get(), EngineEvent::EnterPlay);
        self.shared.engine.set(next);
        // Cancel densify hard — do **not** rearm `scrub_pending` (Play must not
        // keep filling gaps). Keep scrub *markers*; trim Exact (ImageBitmap) by
        // Play VF VRAM estimate — evict farthest from playhead, do not wipe all
        // (was: full clear → cold Exact on every Pause).
        self.cancel_ops();
        self.abort_scrub_fill(false);
        self.clear_scrub_direction();
        self.trim_exact_for_play();
        // Free Chromium VideoFrame pool for prefetch — step ring is Pause/Prev only.
        self.clear_step_behind_ring();
        {
            let player = self.shared.player.borrow();
            let head = player.state().frame;
            let track = player.track_rc();
            if !track.fragments_complete() {
                // Pump mfra index well ahead of play so we never stall at the
                // warm+chunk seam (was solid lag on frame 119 / indexed 120).
                track.request_index_through(head.saturating_add(240));
            }
            // Warm `mdat` for the next ~5s even when the index is already ahead
            // (mfra backfill does not fetch sample bytes).
            track.prefetch_samples_ahead(head, 120);
        }
        self.shared.player.borrow_mut().play();
        self.shared.frames_skipped.set(0);
        self.shared.last_behind_ms.set(None);
        self.shared.underrun_holds.set(0);
        self.shared.underrun_hold_ms.set(0.0);
        self.shared.underrun_hold_since_ms.set(None);
        #[cfg(feature = "thumbs")]
        self.schedule_thumb_key_fill();
    }

    pub fn pause(&self) {
        self.pause_inner(false);
    }

    /// Leave play for timeline scrub — soft-hold audio (no `AudioDecoder` reset).
    ///
    /// Hard [`Self::pause`] still tears down audio (Pause button / open / EOF).
    /// `mediabunny: examples/media-player/media-player.ts` `seekToTime` → `pause`
    /// then `play` without reconfigure.
    pub fn pause_for_scrub(&self) {
        self.pause_inner(true);
    }

    fn pause_inner(&self, soft_audio: bool) {
        // Leaving play: cancel decode + scrub fill epoch (pending latch restarts fill).
        // Already paused: do not bump scrub_epoch (UI steps call pause every time).
        let was_playing = self.shared.player.borrow().state().playing;
        let (next, _) = engine_reduce(
            self.shared.engine.get(),
            EngineEvent::EnterScrub { soft_audio },
        );
        self.shared.engine.set(next);
        if was_playing {
            self.cancel_ops_and_scrub_fill();
            if soft_audio {
                let _ = self.hold_media_clock_for_scrub();
            } else {
                let _ = self.stop_media_clock();
            }
            self.finish_underrun_hold_timer();
        }
        self.shared.player.borrow_mut().pause();
        self.clear_loop_prime();
        // `prepare_scrub_settle` latches ExactHold *before* the async scrub op;
        // `run_seek_or_step` always called `pause()` which used to clear that latch
        // and re-kick densify — exact settle then raced the scrub decoder
        // (`decoder busy` / play-after-scrub resume fail on URL fMP4).
        if self.shared.engine.get().holds_exact() {
            return;
        }
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
                if shared.engine.get().holds_exact()
                    || shared.engine.get().is_play()
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
            presentation_cap: self.play_presentation_cap(),
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
            fetch_file_ranges: self.shared.fetch_map.file_fractions(self.shared.file_size),
        }
    }

    pub fn frame_duration_seconds(&self) -> Result<f64, MediaError> {
        self.duration_of(self.state().frame)
    }

    fn duration_of(&self, frame: FrameId) -> Result<f64, MediaError> {
        let player = self.shared.player.borrow();
        let index = player.track().index();
        let entry = index.get(frame).ok_or(MediaError::FrameOutOfRange(
            frame,
            player.state().frame_count,
        ))?;
        if entry.duration_ticks == 0 || entry.timescale == 0 {
            return Err(MediaError::MissingFrameDuration(frame));
        }
        Ok(entry.duration_ticks as f64 / entry.timescale as f64)
    }

    pub fn frame_timestamp_seconds(&self, frame: FrameId) -> Result<f64, MediaError> {
        let player = self.shared.player.borrow();
        let index = player.track().index();
        let entry = index.get(frame).ok_or(MediaError::FrameOutOfRange(
            frame,
            player.state().frame_count,
        ))?;
        Ok(entry.composition_timestamp().seconds)
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
        let cap = self.play_presentation_cap();
        while presentation.len() > cap {
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
        if self.shared.engine.get().is_play() {
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
            if self.shared.engine.get().is_play() {
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

    fn clear_loop_prime(&self) {
        let taken = self.shared.loop_prime.borrow_mut().take();
        if let Some(buf) = taken {
            for q in buf {
                drop(q.video);
            }
        }
    }

    fn apply_decoder_slot(&self, event: DecoderSlotEvent) -> Vec<DecoderSlotCmd> {
        let (next, cmds) = decoder_slot_reduce(self.shared.decoder_slot.get(), event);
        self.shared.decoder_slot.set(next);
        cmds
    }

    fn invalidate_decoder(&self) {
        // Soft clear is not enough: clearing `input_timestamps` while the browser
        // still holds in-flight chunks desyncs PTS remap → rare
        // `flush/idle without pts=… (held=0)` on rapid PREV.
        // mediabunny: terminate iterator → close decoder; next range creates fresh.
        let cmds = self.apply_decoder_slot(DecoderSlotEvent::Invalidate);
        for cmd in cmds {
            match cmd {
                DecoderSlotCmd::ResetNow => {
                    // Take out so `reset()` does not hold the RefCell (JS re-entry / Drop).
                    let taken = self.decoder.borrow_mut().take();
                    if let Some(mut decoder) = taken {
                        if decoder.reset().is_ok() {
                            let _ = self.apply_decoder_slot(DecoderSlotEvent::ClearReset);
                        }
                        *self.decoder.borrow_mut() = Some(decoder);
                    }
                }
                DecoderSlotCmd::ApplyReset => {}
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
            let _ = self.apply_decoder_slot(DecoderSlotEvent::ClearReset);
            self.put_decoder(decoder);
            if let Ok(video) = result {
                drop(video);
            }
            return Err(MediaError::Cancelled);
        }
        self.put_decoder(decoder);
        result
    }

    fn put_decoder(&self, decoder: WebCodecsDecoder) {
        let _ = self.apply_decoder_slot(DecoderSlotEvent::PutBack);
        *self.decoder.borrow_mut() = Some(decoder);
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
            if let Some(mut decoder) = taken {
                let cmds = self.apply_decoder_slot(DecoderSlotEvent::Checkout);
                for cmd in cmds {
                    if matches!(cmd, DecoderSlotCmd::ApplyReset) {
                        decoder.reset()?;
                        let _ = self.apply_decoder_slot(DecoderSlotEvent::ClearReset);
                    }
                }
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
        if self.is_playing() && self.shared.engine.get().is_play() {
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
}

impl Drop for VideoPlayer {
    fn drop(&mut self) {
        self.clear_loop_prime();
        self.clear_presentation();
        self.clear_step_ring();
        self.clear_scrub_store();
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

/// Await one animation frame with a dedicated Closure (not Promise.resolve).
///
/// Used to pace densify between GOPs so Chrome's GPU process can reclaim after
/// a burst of `transferToImageBitmap`. Do not pass `resolve` directly to rAF.
async fn wait_animation_frame() {
    let promise = js_sys::Promise::new(&mut |resolve, _reject| {
        let Some(window) = web_sys::window() else {
            let _ = resolve.call0(&wasm_bindgen::JsValue::NULL);
            return;
        };
        let resolve = resolve.clone();
        let cb = Closure::once(move |_t: f64| {
            let _ = resolve.call0(&wasm_bindgen::JsValue::NULL);
        });
        let _ = window.request_animation_frame(cb.as_ref().unchecked_ref());
        cb.forget();
    });
    let _ = wasm_bindgen_futures::JsFuture::from(promise).await;
}

async fn sleep_ms(ms: i32) {
    let promise = js_sys::Promise::new(&mut |resolve, _reject| {
        let _ = web_sys::window()
            .expect("window")
            .set_timeout_with_callback_and_timeout_and_arguments_0(&resolve, ms);
    });
    let _ = wasm_bindgen_futures::JsFuture::from(promise).await;
}
