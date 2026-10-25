//! Open / construct + background index workers for [`super::VideoPlayer`].
//!
//! Split from `mod.rs` for navigation only — no behavior change (P4).
//! `mediabunny:` UrlSource open / readFragment bg; deferred sync on retrieve.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::rc::Rc;

use web_sys::HtmlCanvasElement;

use crate::MediaClock;
use crate::audio::AudioOutput;
use crate::decode::canvas::{canvas_2d, configure_canvas_for_video, reset_canvas_layout};
use crate::decode::frame_cache::ScrubBlitLru;
use crate::decode::scrub_blit::BlitCanvasSlot;
use crate::decode::webcodecs::WebCodecsDecoder;
use crate::error::MediaError;
use crate::model::clock_state::ClockState;
use crate::model::decoder_slot::DecoderSlotModel;
use crate::model::engine_mode::EngineModel;
use crate::model::session::SessionModel;
use crate::policy::ThumbPolicy;
use crate::policy::workload::{
    FRAGMENT_BACKFILL_IDLE_AHEAD, allows_bg_sync_verify, classify,
    fragment_backfill_should_idle_yield,
};
use crate::residency::ScrubChunkStore;

use super::scrub::apply_clip_budgets;
use super::shared::Shared;
use super::{VideoPlayer, sleep_ms};

impl VideoPlayer {
    pub async fn open_mp4_bytes(
        bytes: Vec<u8>,
        canvas: HtmlCanvasElement,
    ) -> Result<Self, MediaError> {
        let opened = crate::open_mp4_bytes(bytes)?;
        let video = opened.video.ok_or(MediaError::NoVideoTrack)?;
        Self::from_tracks_with_fetch(video, opened.audio, canvas, opened.fetch_map).await
    }

    /// Progressive open from a ranged [`crate::MediaSource`] (Blob / memory).
    pub async fn open_mp4_source(
        source: std::sync::Arc<dyn crate::MediaSource>,
        canvas: HtmlCanvasElement,
    ) -> Result<Self, MediaError> {
        let opened = crate::open_mp4(
            source,
            crate::OpenMp4Options {
                verify_avc_sync: true,
                defer_avc_sync: true,
            },
            |_| {},
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
        if track.frame_count() == 0 {
            return Err(MediaError::EmptyTrack);
        }
        let file_size = track.source().size();
        Self::from_track_rc(Rc::new(track), audio, canvas, fetch_map, file_size).await
    }

    async fn from_track_rc(
        track: Rc<crate::demux::Mp4VideoTrack>,
        audio_track: Option<crate::audio::Mp4AudioTrack>,
        canvas: HtmlCanvasElement,
        fetch_map: std::sync::Arc<crate::FetchMap>,
        file_size: u64,
    ) -> Result<Self, MediaError> {
        let meta = track.metadata();
        // Shared stage canvas keeps the previous clip's attrs/aspect until reset.
        reset_canvas_layout(&canvas);
        configure_canvas_for_video(&canvas, meta.width, meta.height)?;
        let decoder = WebCodecsDecoder::create(&meta).await?;
        let ctx = canvas_2d(&canvas)?;
        let max_frames = meta.scrub_cache_frame_budget();
        let audio = match audio_track {
            Some(t) => match AudioOutput::create(t).await {
                Ok(a) => {
                    web_sys::console::log_1(
                        &format!(
                            "fpmedia-web: audio track ready ({}, {} Hz, {} ch)",
                            a.metadata().codec.codec_string.as_deref().unwrap_or("?"),
                            a.metadata().sample_rate,
                            a.metadata().number_of_channels
                        )
                        .into(),
                    );
                    Some(Rc::new(a))
                }
                Err(e) => {
                    web_sys::console::warn_1(&format!("fpmedia-web: audio disabled ({e})").into());
                    None
                }
            },
            None => {
                web_sys::console::log_1(&"fpmedia-web: no audio track".into());
                None
            }
        };
        let playback_clock = if audio.is_none() {
            match crate::audio::PlaybackClock::create() {
                Ok(c) => {
                    web_sys::console::log_1(
                        &"fpmedia-web: PlaybackClock (silent AudioContext) ready".into(),
                    );
                    Some(Rc::new(c))
                }
                Err(e) => {
                    web_sys::console::warn_1(
                        &format!("fpmedia-web: PlaybackClock disabled ({e}); wall clock fallback")
                            .into(),
                    );
                    None
                }
            }
        } else {
            None
        };
        let shared = Rc::new(Shared {
            track,
            frame: Cell::new(0),
            playing: Cell::new(false),
            looping: Cell::new(false),
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
            scrub_cache_policy: Cell::new(crate::policy::ScrubCachePolicy::On),
            scrub_densify_blocked: Cell::new(false),
            scrub_epoch: Cell::new(0),
            densify: crate::SingleFlight::new(),
            gray_fill: crate::SingleFlight::new(),
            scrub_bg_suspended: Cell::new(false),
            underrun_catchup_until_ms: Cell::new(None),
            loop_origin_warm_started: Cell::new(false),
            scrub_gesture: Cell::new(false),
            engine: Cell::new(EngineModel::new()),
            canvas,
            ctx,
            frames_skipped: Cell::new(0),
            last_behind_ms: Cell::new(None),
            underrun_holds: Cell::new(0),
            underrun_hold_ms: Cell::new(0.0),
            underrun_hold_since_ms: Cell::new(None),
            clock_state: Cell::new(ClockState::Stopped),
            session: Cell::new(SessionModel::new()),
            play_phase: Cell::new(crate::PlayPhase::Idle),
            play_iter: RefCell::new(None),
            play_next: RefCell::new(None),
            play_pool: RefCell::new(None),
            play_updating: Cell::new(false),
            pending_silent_clock: Cell::new(false),
            resuming_clock: Cell::new(false),
            held_media_origin: Cell::new(None),
            play_driver_error: RefCell::new(None),
            op_epoch: Cell::new(0),
            decoder_slot: Cell::new(DecoderSlotModel::new()),
            fill_stalled_aim: Cell::new(None),
            pending_cold_exact: Cell::new(None),
            scrub_dir: Cell::new(0),
            prev_scrub_aim: Cell::new(None),
            audio: RefCell::new(audio),
            playback_clock,
            clock: RefCell::new(MediaClock::new()),
            wall_origin: Cell::new(None),
            fetch_map,
            file_size,
            disposed: Cell::new(false),
        });
        apply_clip_budgets(&shared);
        if let Some(a) = shared.audio.borrow().as_ref() {
            shared
                .track
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
        let track = shared.track.clone();
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
                "fpmedia-web: fragment backfill bg (indexed={}/{})",
                track.indexed_frame_count(),
                track.frame_count()
            )
            .into(),
        );
        let track = track.clone();
        // Weak — must not pin `Shared` (AudioContext) across clip switch.
        let weak = Rc::downgrade(shared);
        wasm_bindgen_futures::spawn_local(async move {
            loop {
                let Some(shared) = weak.upgrade() else {
                    break;
                };
                if shared.is_disposed() {
                    if let Some(out) = shared.audio.borrow().as_ref() {
                        out.set_expect_more_samples(false);
                    }
                    break;
                }
                match track.run_fragment_backfill_step().await {
                    Ok((more, audio)) => {
                        let Some(shared) = weak.upgrade() else {
                            break;
                        };
                        if shared.is_disposed() {
                            if let Some(out) = shared.audio.borrow().as_ref() {
                                out.set_expect_more_samples(false);
                            }
                            break;
                        }
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
                                    "fpmedia-web: fragment backfill done ({} frames)",
                                    track.frame_count()
                                )
                                .into(),
                            );
                            break;
                        }
                        web_sys::console::log_1(
                            &format!(
                                "fpmedia-web: fragment backfill chunk (indexed={}/{})",
                                track.indexed_frame_count(),
                                track.frame_count()
                            )
                            .into(),
                        );
                        // Keep index well ahead of the playhead; only idle-yield when
                        // paused/scrubbing with ≥240 frames of lead.
                        let mode = classify(shared.workload_hints());
                        let playhead = shared.frame.get();
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
                        drop(shared);
                        crate::util::yield_now().await;
                    }
                    Err(e) => {
                        if let Some(shared) = weak.upgrade()
                            && let Some(out) = shared.audio.borrow().as_ref()
                        {
                            out.set_expect_more_samples(false);
                        }
                        web_sys::console::error_1(
                            &format!("fpmedia-web: fragment backfill failed: {e}").into(),
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
        let track = shared.track.clone();
        if track.sync_verify_complete() {
            return;
        }
        let pending = track.sync_verify_pending();
        web_sys::console::log_1(
            &format!("fpmedia-web: deferred sync verify bg ({pending} candidates)").into(),
        );
        let weak = Rc::downgrade(shared);
        wasm_bindgen_futures::spawn_local(async move {
            loop {
                let Some(shared) = weak.upgrade() else {
                    break;
                };
                if shared.is_disposed() {
                    break;
                }
                // Yield while Play / Seek own the network, on underrun / scrub
                // soft-hold, or while densify is busy (avoid GitHub/proxy 502 +
                // sample-sized Range storms). Tab-hidden scrub yield does not block verify.
                // mediabunny: verifyKeyPackets on retrieve — no bg full-file walk
                // during playback (`WorkloadMode` / `allows_bg_sync_verify`).
                if !allows_bg_sync_verify(shared.workload_hints()) {
                    // Long park — not `sleep_ms(50)` (20 wakeups/s while Play owns
                    // the clock). mediabunny: no bg sync walk during playback.
                    sleep_ms(1000).await;
                    continue;
                }
                let track = shared.track.clone();
                if track.sync_verify_complete() {
                    break;
                }
                match track.verify_next_pending().await {
                    Ok(true) => crate::util::yield_now().await,
                    Ok(false) => {
                        web_sys::console::log_1(&"fpmedia-web: deferred sync verify done".into());
                        break;
                    }
                    Err(e) => {
                        web_sys::console::warn_1(
                            &format!("fpmedia-web: deferred sync verify stopped ({e})").into(),
                        );
                        break;
                    }
                }
            }
        });
    }
}
