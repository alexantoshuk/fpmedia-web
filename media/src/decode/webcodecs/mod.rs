//! WebCodecs `VideoDecoder` wrapper.
//!
//! Playback follows mediabunny's model (`mediabunny: src/media-sink.ts`
//! `mediaSamplesInRange`): a **pump** task feeds packets in decode order while a
//! **consumer** task pulls the target frame out of the presentation buffer. The
//! two run concurrently, so `decodeQueueSize` backpressure unblocks the way
//! mediabunny expects — `onQueueDequeue` fires on a consumer take, not only on
//! decoder output. Prefer feeding further ahead in the GOP over `flush()`.
//!
//! All decoder state lives in [`DecoderCore`] behind `Rc<RefCell<…>>` so pump and
//! consumer can hold it alternately; never keep a borrow across `.await`,
//! `decoder.decode()`, or a `Closure` call (`.ai/skills/media.md`).
//!
//! Layout: [`core`] owns the synchronous decoder state, [`pump`] the async
//! feed / drain helpers, and this module the public [`WebCodecsDecoder`] API.

mod core;
mod pump;

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use wasm_bindgen_futures::{JsFuture, spawn_local};
use web_sys::VideoDecoder;

use crate::decode::owned_video_frame::OwnedVideoFrame;
use crate::demux::Mp4VideoTrack;
use crate::error::{DecodeStall, MediaError};
use crate::policy::{GopProfile, PlayPolicy};
use crate::types::{FrameId, VideoMetadata};

use self::core::{
    DecoderConfig, DecoderCore, HwAccel, PruneFloorGuard, PruneInterim, REORDER_FEED_SLACK,
    ensure_webcodecs_available,
};
use self::pump::{
    FramePump, LivePump, PumpPlayCursor, PumpSignals, PumpStopGuard, await_outputs_if_pending,
    feed_sample, feed_sample_gop, finish_gop_for_target, flush_with_timeout, flush_with_timeout_ms,
    holds_target, is_sequential_next_key, now_ms, poll_for_target, progress_signature,
    pts_aim_hole, sleep_promise_ms, ticks_to_micros, wait_for_frame_progress,
};

/// Hard backstop for the consumer pull loop when no clock is available.
const CONSUMER_MAX_SPINS: u32 = 20_000;
/// Consumer gives up when nothing observable changes for this long
/// (`held`, `decodeQueueSize`, raw outputs, fed packets, pump heartbeat).
const CONSUMER_STALL_MS: f64 = 5_000.0;

/// Decode presentation frames via WebCodecs with a warm decode-order pump.
///
/// Thin shell over [`DecoderCore`]: the spawned packet pump keeps its own `Rc`
/// to the core plus `pump_epoch`, so a reset/detach/Drop here stops it even
/// though it outlives this value's borrow.
pub struct WebCodecsDecoder {
    core: Rc<RefCell<DecoderCore>>,
    pump_epoch: Rc<Cell<u64>>,
    /// Live range feeder cursor — retargeted on consumer take (no respawn).
    play_cursor: Rc<PumpPlayCursor>,
    /// Last spawned pump; skip respawn while this epoch is still feeding.
    live_pump: RefCell<Option<LivePump>>,
}

impl WebCodecsDecoder {
    /// Main play/seek decoder — knobs from [`VideoMetadata::play_policy`].
    ///
    /// Always probes `prefer-hardware` when the host supports it (Hd and Uhd).
    /// Scrub/thumb stay [`HwAccel::PreferSoftware`] via [`Self::create_scrub`].
    /// mediabunny: `decoderOptions.hardwareAcceleration` (PR #406).
    pub async fn create(meta: &VideoMetadata) -> Result<Self, MediaError> {
        // `true`: HW-only codecs (HEVC) + prefer HW for main play when available.
        Self::create_with_policy(meta, meta.play_policy(), true).await
    }

    /// Scrub densify decoder — always [`PlayPolicy::hd`] knobs and explicit
    /// `prefer-software` so the main session keeps the sole HW decoder
    /// (mediabunny: `src/media-sink.ts` alpha `prefer-software`; dual-HW /
    /// `Unsupported configuration` when a second decoder steals HW).
    pub async fn create_scrub(meta: &VideoMetadata) -> Result<Self, MediaError> {
        // Hd knobs (cap/warm) only — force PreferSoftware even if main prefers HW.
        Self::create_with_policy(meta, PlayPolicy::hd(), false).await
    }

    /// Hover / filmstrip thumb decoder — same isolation as [`Self::create_scrub`]
    /// (Hd knobs, PreferSoftware). Separate long-lived sink from play + densify.
    /// `mediabunny:` `CanvasSink` + own `VideoSampleSink` / `getCanvas`.
    pub async fn create_thumb(meta: &VideoMetadata) -> Result<Self, MediaError> {
        Self::create_scrub(meta).await
    }

    /// `allow_hw` — main play/seek passes `true`: (1) if no-preference
    /// `isConfigSupported` is false, retry `prefer-hardware` (HEVC); (2) if
    /// soft-ok, still prefer HW when the host supports it. Scrub/thumb pass
    /// `false` → probe+configure [`HwAccel::PreferSoftware`] only.
    async fn create_with_policy(
        meta: &VideoMetadata,
        policy: PlayPolicy,
        allow_hw: bool,
    ) -> Result<Self, MediaError> {
        ensure_webcodecs_available()?;
        let config = DecoderConfig::from_meta(meta)?;
        // Prefer throughput configure — `optimizeForLatency: true` wedges Chromium
        // on sustained play/deep key→N (`backpressure q≈41` / `flush/idle`).
        // mediabunny leaves this optional; we set false for VOD scrub+play.
        let optimize_for_latency = false;

        // Scrub/thumb: PreferSoftware only — never no-preference (Chrome may still
        // grab HW) and never prefer-hardware (dual-HW stall / poison main).
        // mediabunny: src/media-sink.ts VideoDecoderWrapper alpha path.
        if !allow_hw {
            let probe = config.to_web_config(optimize_for_latency, HwAccel::PreferSoftware);
            web_sys::console::log_1(
                &format!(
                    "webmedia: VideoDecoder isConfigSupported ({}, {}x{}, play={}, hw=prefer-software)…",
                    config.codec,
                    config.width,
                    config.height,
                    policy.label()
                )
                .into(),
            );
            let support = JsFuture::from(VideoDecoder::is_config_supported(&probe))
                .await
                .map_err(MediaError::from)?;
            let supported = js_sys::Reflect::get(&support, &"supported".into())
                .ok()
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            if !supported {
                web_sys::console::log_1(
                    &"webmedia: scrub densify skipped (codec HW-only; refuse second HW decoder)"
                        .into(),
                );
                return Err(MediaError::CodecUnsupported(config.codec.clone()));
            }
            let pump_epoch = Rc::new(Cell::new(0));
            let all_intra = matches!(meta.gop_profile, GopProfile::AllIntra);
            let core = DecoderCore::from_config(
                config,
                optimize_for_latency,
                HwAccel::PreferSoftware,
                policy.decode_warm_ahead,
                Rc::clone(&pump_epoch),
                all_intra,
            )?;
            web_sys::console::log_1(
                &format!(
                    "webmedia: VideoDecoder configured (warm={}, hw=prefer-software)",
                    policy.decode_warm_ahead
                )
                .into(),
            );
            return Ok(Self {
                core: Rc::new(RefCell::new(core)),
                pump_epoch,
                play_cursor: Rc::new(PumpPlayCursor::default()),
                live_pump: RefCell::new(None),
            });
        }

        // Main: probe without prefer-hardware first (AVC software OK). HEVC on
        // Chrome is often HW-only — if no-preference says unsupported, retry with
        // prefer-hardware. Do not *require* HW on the first probe.
        // mediabunny: decoderOptions.hardwareAcceleration.
        let probe = config.to_web_config(optimize_for_latency, HwAccel::NoPreference);
        web_sys::console::log_1(
            &format!(
                "webmedia: VideoDecoder isConfigSupported ({}, {}x{}, play={}, allow_hw=true)…",
                config.codec,
                config.width,
                config.height,
                policy.label()
            )
            .into(),
        );
        let support = JsFuture::from(VideoDecoder::is_config_supported(&probe))
            .await
            .map_err(MediaError::from)?;
        let mut supported = js_sys::Reflect::get(&support, &"supported".into())
            .ok()
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let mut hw_accel = HwAccel::NoPreference;
        if !supported {
            let hw_probe = config.to_web_config(optimize_for_latency, HwAccel::PreferHardware);
            let hw_support = JsFuture::from(VideoDecoder::is_config_supported(&hw_probe))
                .await
                .map_err(MediaError::from)?;
            let hw_ok = js_sys::Reflect::get(&hw_support, &"supported".into())
                .ok()
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            if hw_ok {
                web_sys::console::log_1(
                    &"webmedia: no-preference unsupported; prefer-hardware ok (HEVC/HW-only)"
                        .into(),
                );
                supported = true;
                hw_accel = HwAccel::PreferHardware;
            }
        }
        if !supported {
            return Err(MediaError::CodecUnsupported(config.codec.clone()));
        }

        // Main play: prefer HW whenever the host supports it (Hd + Uhd).
        // mediabunny: decoderOptions.hardwareAcceleration (PR #406).
        if hw_accel != HwAccel::PreferHardware {
            let hw_probe = config.to_web_config(optimize_for_latency, HwAccel::PreferHardware);
            let hw_support = JsFuture::from(VideoDecoder::is_config_supported(&hw_probe))
                .await
                .map_err(MediaError::from)?;
            let hw_ok = js_sys::Reflect::get(&hw_support, &"supported".into())
                .ok()
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            if hw_ok {
                hw_accel = HwAccel::PreferHardware;
                web_sys::console::log_1(&"webmedia: VideoDecoder prefer-hardware ok".into());
            } else {
                web_sys::console::warn_1(
                    &"webmedia: prefer-hardware unsupported; no-preference configure".into(),
                );
            }
        }

        let pump_epoch = Rc::new(Cell::new(0));
        let all_intra = matches!(meta.gop_profile, GopProfile::AllIntra);
        let core = DecoderCore::from_config(
            config,
            optimize_for_latency,
            hw_accel,
            policy.decode_warm_ahead,
            Rc::clone(&pump_epoch),
            all_intra,
        )?;
        web_sys::console::log_1(
            &format!(
                "webmedia: VideoDecoder configured (warm={}, hw={})",
                policy.decode_warm_ahead,
                hw_accel.label()
            )
            .into(),
        );
        Ok(Self {
            core: Rc::new(RefCell::new(core)),
            pump_epoch,
            play_cursor: Rc::new(PumpPlayCursor::default()),
            live_pump: RefCell::new(None),
        })
    }

    pub fn invalidate(&mut self) {
        // Prefer a hard decoder reset — soft clear of timestamps/held while the
        // browser still has in-flight chunks desyncs PTS remap.
        let _ = self.reset();
    }

    /// Close and recreate the VideoDecoder; clear warm GOP / held / timestamps.
    /// Stops any in-flight pump (`pump_epoch` bump inside `detach_decoder`).
    pub fn reset(&mut self) -> Result<(), MediaError> {
        self.core.borrow_mut().recreate()
    }

    /// In-flight encoded chunks waiting in the browser decoder.
    pub fn browser_decode_queue_size(&self) -> u32 {
        self.core.borrow().decode_queue_size()
    }

    /// Decoded frames parked for presentation-order pull (B-frame reorder).
    pub fn held_len(&self) -> usize {
        self.core.borrow().held.len()
    }

    /// Decode until `frame` is available, feeding decode-order packets as needed.
    ///
    /// `is_cancelled` — host op epoch (rapid PREV/seek); bail with [`MediaError::Cancelled`].
    pub async fn decode_frame(
        &mut self,
        track: &Rc<Mp4VideoTrack>,
        frame: FrameId,
        is_cancelled: &dyn Fn() -> bool,
    ) -> Result<OwnedVideoFrame, MediaError> {
        self.decode_frame_inner(track, frame, is_cancelled).await
    }

    /// Like [`Self::decode_frame`], but salvages pruned PTS `< target` frames to
    /// `on_interim` (dense insert only — do not blit). Avoids a second full-GOP
    /// densify pass after long-GOP settle/seek.
    pub async fn decode_frame_with_interim(
        &mut self,
        track: &Rc<Mp4VideoTrack>,
        frame: FrameId,
        is_cancelled: &dyn Fn() -> bool,
        on_interim: Box<dyn FnMut(FrameId, OwnedVideoFrame)>,
    ) -> Result<OwnedVideoFrame, MediaError> {
        let key_id = track.ensure_key_verified(frame).await?;
        if is_cancelled() {
            return Err(MediaError::Cancelled);
        }
        let target_us = {
            let index = track.index();
            let entry = index
                .get(frame)
                .ok_or(MediaError::FrameOutOfRange(frame, track.frame_count()))?;
            ticks_to_micros(entry.composition_timestamp_ticks, entry.timescale)
        };
        let gop_ids = track.index().gop_frame_ids(key_id).unwrap_or_default();
        let mut pts_to_id: HashMap<i64, FrameId> = HashMap::with_capacity(gop_ids.len());
        {
            let index = track.index();
            for id in &gop_ids {
                let e = index
                    .get(*id)
                    .ok_or(MediaError::FrameOutOfRange(*id, track.frame_count()))?;
                pts_to_id.insert(
                    ticks_to_micros(e.composition_timestamp_ticks, e.timescale),
                    *id,
                );
            }
        }
        *self.core.borrow().prune_interim.borrow_mut() = Some(PruneInterim {
            pts_to_id,
            target_us,
            on_frame: on_interim,
        });
        let result = self.decode_frame_inner(track, frame, is_cancelled).await;
        *self.core.borrow().prune_interim.borrow_mut() = None;
        result
    }

    async fn decode_frame_inner(
        &mut self,
        track: &Rc<Mp4VideoTrack>,
        frame: FrameId,
        is_cancelled: &dyn Fn() -> bool,
    ) -> Result<OwnedVideoFrame, MediaError> {
        if is_cancelled() {
            return Err(MediaError::Cancelled);
        }
        let core = Rc::clone(&self.core);
        // On-demand bitstream verify (deferred open). mediabunny: verifyKeyPackets on retrieve.
        let key_id = track.ensure_key_verified(frame).await?;
        if is_cancelled() {
            return Err(MediaError::Cancelled);
        }
        let (target_us, target_do, key_do) = {
            let index = track.index();
            let entry = index
                .get(frame)
                .ok_or(MediaError::FrameOutOfRange(frame, track.frame_count()))?;
            let key_do = index
                .get(key_id)
                .map(|e| e.decode_order)
                .ok_or(MediaError::FrameOutOfRange(key_id, track.frame_count()))?;
            (
                ticks_to_micros(entry.composition_timestamp_ticks, entry.timescale),
                entry.decode_order,
                key_do,
            )
        };
        // Protect aim from live-cursor prune races (Chrome: near=83333 wiped 41666).
        let _prune_floor = PruneFloorGuard::arm(&core, target_us);

        {
            let mut c = core.borrow_mut();
            c.collect_and_prune_held(target_us)?;
            if let Some(ready) = c.take_held(target_us) {
                let ahead = c.held.iter().filter(|h| h.pts_us > target_us).count();
                let want = c.warm_ahead;
                drop(c);
                // Hit in held — still keep a cross-GOP pump warm so Play does not
                // drain the buffer dry between takes (was: return with no feeder).
                // Retarget live feeder; do not bump epoch while it still runs
                // (mediabunny: mediaSamplesInRange — one loop, consumer pulls).
                if ahead < want {
                    self.ensure_warm_pump(Rc::clone(track), key_do, target_do, target_us);
                } else {
                    self.retarget_play_cursor(target_do, target_us);
                }
                return Ok(ready);
            }
        }

        // Warm trail has later PTS but not the aim (often after continuous ChG
        // park + wiped `fed`) — hard-recreate before a range pump feeds to EOF.
        // mediabunny: seekToTime → await videoFrameIterator?.return() then new range.
        if pts_aim_hole(&core.borrow(), target_us, target_do) {
            self.run_key_cascade(
                track,
                frame,
                key_id,
                key_do,
                target_us,
                true,
                false,
                is_cancelled,
            )
            .await?;
            let mut c = core.borrow_mut();
            c.collect_and_prune_held(target_us)?;
            if let Some(ready) = c.take_held(target_us) {
                return Ok(ready);
            }
        }

        // Warm feed-ahead may have fed past this target while outputs are still
        // in flight. Poll briefly; if the PTS never appears, hard-recreate from
        // the key (do not flush — that demands a key on the next delta and still
        // misses when Chromium has wedged mid-GOP).
        let fed_order = core.borrow().fed_through_order;
        if fed_order.is_some_and(|fed| target_do <= fed) {
            for _ in 0..24 {
                if is_cancelled() {
                    return Err(MediaError::Cancelled);
                }
                {
                    let mut c = core.borrow_mut();
                    c.collect_and_prune_held(target_us)?;
                    if let Some(ready) = c.take_held(target_us) {
                        let ahead = c.held.iter().filter(|h| h.pts_us > target_us).count();
                        let want = c.warm_ahead;
                        drop(c);
                        if ahead < want {
                            self.ensure_warm_pump(Rc::clone(track), key_do, target_do, target_us);
                        } else {
                            self.retarget_play_cursor(target_do, target_us);
                        }
                        return Ok(ready);
                    }
                    let idle = c.decode_queue_size() == 0
                        && !c.raw_outputs_pending()
                        && c.input_timestamps.is_empty();
                    if idle {
                        break;
                    }
                }
                await_outputs_if_pending(&core).await?;
                wait_for_frame_progress(&core).await;
            }
            if !holds_target(&core, target_us)
                && (pts_aim_hole(&core.borrow(), target_us, target_do)
                    || fed_order
                        .is_some_and(|fed| fed.saturating_sub(target_do) >= REORDER_FEED_SLACK))
            {
                self.run_key_cascade(
                    track,
                    frame,
                    key_id,
                    key_do,
                    target_us,
                    true,
                    false,
                    is_cancelled,
                )
                .await?;
                let mut c = core.borrow_mut();
                c.collect_and_prune_held(target_us)?;
                if let Some(ready) = c.take_held(target_us) {
                    return Ok(ready);
                }
            }
        }

        // Restart when the GOP changes, or when the target was already emitted and
        // discarded (e.g. presentation queue cleared after prefetch) — WebCodecs
        // cannot re-emit a consumed frame without cascading from the key again.
        // Also treat idle later-only `held` as consumed when `fed` was wiped
        // (continuous park); otherwise needs_cascade stays false and the pump
        // EOFs the short B-frame clip (`flush/idle` held=N-2).
        let (needs_cascade, target_consumed, sequential_next) = {
            let c = core.borrow();
            let hole = pts_aim_hole(&c, target_us, target_do);
            let consumed = hole
                || c.fed_through_order.is_some_and(|fed| {
                    target_do <= fed
                        && !c
                            .input_timestamps
                            .iter()
                            .any(|&t| t == target_us || (t - target_us).abs() <= 1)
                });
            let sequential = c
                .key_order
                .is_some_and(|prev| is_sequential_next_key(track, prev, key_do));
            (
                c.key_order != Some(key_do) || consumed,
                consumed,
                sequential,
            )
        };
        if needs_cascade {
            self.run_key_cascade(
                track,
                frame,
                key_id,
                key_do,
                target_us,
                target_consumed,
                sequential_next,
                is_cancelled,
            )
            .await?;
            let mut c = core.borrow_mut();
            c.collect_and_prune_held(target_us)?;
            if let Some(ready) = c.take_held(target_us) {
                return Ok(ready);
            }
        }

        // Before dual-async: if we already fed past the aim without holding it,
        // hard-cascade again — a mid-GOP resume EOFs without the PTS.
        if !holds_target(&core, target_us) {
            let must_rewind = {
                let c = core.borrow();
                pts_aim_hole(&c, target_us, target_do)
                    || c.fed_through_order.is_some_and(|fed| {
                        fed >= target_do
                            && !c
                                .input_timestamps
                                .iter()
                                .any(|&t| t == target_us || (t - target_us).abs() <= 1)
                    })
            };
            if must_rewind {
                self.run_key_cascade(
                    track,
                    frame,
                    key_id,
                    key_do,
                    target_us,
                    true,
                    false,
                    is_cancelled,
                )
                .await?;
                let mut c = core.borrow_mut();
                c.collect_and_prune_held(target_us)?;
                if let Some(ready) = c.take_held(target_us) {
                    return Ok(ready);
                }
            }
        }

        // Dual async (mediabunny: src/media-sink.ts `mediaSamplesInRange`): the
        // pump feeds decode-order packets in its own task while this task pulls
        // the target from `held`. Backpressure can now unblock on a consumer
        // take, which is what lets the pump wait at `decodeQueueSize == 0`.
        //
        // Bump epoch first so a prior background warm-fill / stray pump stops
        // before this range starts (only one feeder on the decoder).
        self.retarget_play_cursor(target_do, target_us);
        self.pump_epoch.set(self.pump_epoch.get().wrapping_add(1));
        let pump_epoch_now = self.pump_epoch.get();
        let signals = Rc::new(PumpSignals::default());
        *self.live_pump.borrow_mut() = Some(LivePump {
            signals: Rc::clone(&signals),
            epoch: pump_epoch_now,
        });
        let stop_on_return = PumpStopGuard(Rc::clone(&signals));
        let cursor = Rc::clone(&self.play_cursor);
        spawn_local(
            FramePump {
                core: Rc::clone(&core),
                pump_epoch: Rc::clone(&self.pump_epoch),
                epoch: pump_epoch_now,
                track: Rc::clone(track),
                key_do,
                cursor,
                signals: Rc::clone(&signals),
                warm_only: false,
            }
            .run(),
        );

        let mut spins = 0u32;
        let mut last_sig = progress_signature(&core, &signals);
        let mut last_change_ms = now_ms();
        loop {
            if is_cancelled() {
                return Err(MediaError::Cancelled);
            }
            // Bind out of the RefCell first — no guard across the body.
            let pump_error = signals.error.borrow_mut().take();
            if let Some(err) = pump_error {
                return Err(err);
            }
            let ready = {
                let mut c = core.borrow_mut();
                c.check_error()?;
                c.collect_and_prune_held(target_us)?;
                c.take_held(target_us)
            };
            if let Some(ready) = ready {
                // Return the frame immediately so presentation can present /
                // clear underrun. Keep the range pump alive in the background
                // until warm-ahead is parked — immediate `stop` on take raced
                // backpressure and forced a cold pump every 1–2 frames
                // (long-GOP ~0.75×). mediabunny keeps decoding while the consumer
                // pulls (`src/media-sink.ts` mediaSamplesInRange).
                // Aim follows later takes via `play_cursor` (retarget, no respawn).
                let core_bg = Rc::clone(&core);
                let signals_bg = Rc::clone(&signals);
                let pump_epoch_bg = Rc::clone(&self.pump_epoch);
                let cursor_bg = Rc::clone(&self.play_cursor);
                spawn_local(async move {
                    // Wait until warm-ahead is parked, then leave the feeder
                    // alive (parked). Do **not** `stop` — that killed the
                    // continuous mediabunny-style range loop under Play.
                    let want_ahead = core_bg.borrow().warm_ahead;
                    for _ in 0..80 {
                        if pump_epoch_bg.get() != pump_epoch_now || signals_bg.stop.get() {
                            return;
                        }
                        let aim_us = cursor_bg.target_us.get();
                        let warm = {
                            let mut c = core_bg.borrow_mut();
                            if c.collect_and_prune_held(aim_us).is_err() {
                                return;
                            }
                            c.held.iter().filter(|h| h.pts_us > aim_us).count() >= want_ahead
                                || signals_bg.done.get()
                        };
                        if warm {
                            return;
                        }
                        wait_for_frame_progress(&core_bg).await;
                    }
                });
                // Background owns stop — do not kill the pump on return.
                std::mem::forget(stop_on_return);
                return Ok(ready);
            }
            if signals.done.get() {
                // No more packets are coming — drain, then flush once.
                if let Some(ready) = finish_gop_for_target(&core, target_us, is_cancelled).await? {
                    return Ok(ready);
                }
                return Err(MediaError::DecodeStall {
                    kind: DecodeStall::TargetMissing,
                    detail: format!(
                        "flush/idle without pts={target_us}µs (held={}, q={}, fed={:?}, stamps={}, near={:?})",
                        core.borrow().held.len(),
                        core.borrow().decode_queue_size(),
                        core.borrow().fed_through_order,
                        core.borrow().input_timestamps.len(),
                        {
                            let c = core.borrow();
                            let mut nearest: Option<(i64, i64)> = None;
                            for h in &c.held {
                                let d = (h.pts_us - target_us).abs();
                                if nearest.is_none_or(|(_, best)| d < best) {
                                    nearest = Some((h.pts_us, d));
                                }
                            }
                            nearest.map(|(pts, _)| pts)
                        },
                    ),
                });
            }

            let sig = progress_signature(&core, &signals);
            if sig != last_sig {
                last_sig = sig;
                last_change_ms = now_ms();
            } else if now_ms() - last_change_ms > CONSUMER_STALL_MS {
                return Err(MediaError::DecodeStall {
                    kind: DecodeStall::PumpStalled,
                    detail: format!(
                        "decode pump stalled waiting for pts={target_us}µs (q={}, held={})",
                        core.borrow().decode_queue_size(),
                        core.borrow().held.len()
                    ),
                });
            }
            spins += 1;
            if spins > CONSUMER_MAX_SPINS {
                return Err(MediaError::WebCodecs(format!(
                    "decode pump gave up waiting for pts={target_us}µs"
                )));
            }
            wait_for_frame_progress(&core).await;
        }
    }

    /// Feed the GOP key (plus the All-Intra coax / hard-retry cascade) so the
    /// pump can start from a configured decoder.
    ///
    /// Single-task on purpose: the browser needs the key chunk before any
    /// further packet is useful, and the retry ladder recreates the decoder.
    ///
    /// `sequential_next_key`: Play advancing into the adjacent GOP — soft switch
    /// when idle (no recreate). Scrub jumps keep this false.
    #[allow(clippy::too_many_arguments)]
    async fn run_key_cascade(
        &mut self,
        track: &Rc<Mp4VideoTrack>,
        frame: FrameId,
        key_id: FrameId,
        key_do: usize,
        target_us: i64,
        force_hard: bool,
        sequential_next_key: bool,
        is_cancelled: &dyn Fn() -> bool,
    ) -> Result<(), MediaError> {
        let core = Rc::clone(&self.core);
        // Soft Play seam: wait out warm-ahead decodeQ before soft switch so we
        // never hard-recreate on busy (`begin_key_cascade`). Also covers the
        // case where `finish_gop` cleared `key_order` (sequential flag false but
        // soft via !key_changed). mediabunny: `src/media-sink.ts`
        // mediaSamplesInRange keeps the same decoder across adjacent IDRs.
        if !force_hard {
            let soft_wait = {
                let c = core.borrow();
                let key_changed = matches!(c.key_order, Some(prev) if prev != key_do);
                c.all_intra || sequential_next_key || !key_changed
            };
            if soft_wait {
                for _ in 0..64 {
                    if is_cancelled() {
                        return Err(MediaError::Cancelled);
                    }
                    {
                        let mut c = core.borrow_mut();
                        c.check_error()?;
                        let _ = c.collect_and_prune_held(target_us);
                        // Already have the next-GOP target from a prior pump — done.
                        if c.held
                            .iter()
                            .any(|h| h.pts_us == target_us || (h.pts_us - target_us).abs() <= 1)
                        {
                            c.key_order = Some(key_do);
                            return Ok(());
                        }
                        let idle = c.decode_queue_size() == 0
                            && !c.raw_outputs_pending()
                            && c.input_timestamps.is_empty();
                        if idle {
                            break;
                        }
                    }
                    await_outputs_if_pending(&core).await?;
                    wait_for_frame_progress(&core).await;
                }
            }
        }
        // Soft key switch when idle — All-Intra / sequential Play GOP boundary.
        // Hard recreate only if force_hard or non-adjacent Long-GOP jump.
        core.borrow_mut()
            .begin_key_cascade(key_do, force_hard, sequential_next_key)?;
        // Soft clear wiped held — if we already fed this key in a prior range,
        // still must re-feed after soft wipe (fed_through cleared).
        let key_sample = track.encoded_sample(key_id).await?;
        if is_cancelled() {
            return Err(MediaError::Cancelled);
        }
        feed_sample(&core, &key_sample, true, target_us, is_cancelled).await?;
        // Poll with macrotasks (see `poll_output_or_macrotask`) — microtask-only
        // never saw HW output on long-GOP and forced flush.
        if poll_for_target(&core, target_us, 32, is_cancelled).await? {
            return Ok(());
        }
        // Non-key aim: feed decode-order through the aim before handing off to
        // the dual-async pump. Early return after key-only left a stale
        // `fed_through` past the aim → pump resumed mid-GOP and EOF-filled
        // without PTS (Resolve B-frame frame1 / 41666µs, held≈N-2).
        // mediabunny: mediaSamplesInRange walks packets until the pull timestamp.
        if key_id != frame {
            let mut after = key_do;
            let aim_do = {
                let index = track.index();
                index.get(frame).map(|e| e.decode_order).unwrap_or(key_do)
            };
            while after < aim_do {
                if is_cancelled() {
                    return Err(MediaError::Cancelled);
                }
                let next = track.samples_after_decode(after, 1).await?;
                let Some(sample) = next.into_iter().next() else {
                    break;
                };
                after = sample.decode_order;
                feed_sample(&core, &sample, false, target_us, is_cancelled).await?;
                if poll_for_target(&core, target_us, 16, is_cancelled).await? {
                    return Ok(());
                }
            }
            return Ok(());
        }

        // Prefer feeding one more GOP packet over flush — flush forces the
        // next decode() to be a key (Chromium: "needs a key chunk").
        // All-Intra: GOP is one sample — coax is empty; hard-retry instead of
        // a multi-second flush that pins the decoder slot (→ `decoder busy`).
        let coax = track.samples_after_in_gop(key_do, key_do, 1).await?;
        if let Some(sample) = coax.first() {
            feed_sample(&core, sample, false, target_us, is_cancelled).await?;
            if poll_for_target(&core, target_us, 32, is_cancelled).await? {
                return Ok(());
            }
        } else if !holds_target(&core, target_us) {
            // Sole sync (All-Intra): one hard reset + re-feed, no long flush.
            if is_cancelled() {
                return Err(MediaError::Cancelled);
            }
            core.borrow_mut().recreate()?;
            core.borrow_mut().key_order = Some(key_do);
            let key_sample = track.encoded_sample(key_id).await?;
            if is_cancelled() {
                return Err(MediaError::Cancelled);
            }
            feed_sample(&core, &key_sample, true, target_us, is_cancelled).await?;
            if poll_for_target(&core, target_us, 48, is_cancelled).await? {
                return Ok(());
            }
        }
        if !holds_target(&core, target_us) {
            // Last resort — short flush (not 2s) then clear warm state.
            if is_cancelled() {
                return Err(MediaError::Cancelled);
            }
            let _ = flush_with_timeout_ms(&core, 250).await;
            let mut c = core.borrow_mut();
            c.collect_and_prune_held(target_us)?;
            c.fed_through_order = None;
            c.key_order = None;
            c.input_timestamps.clear();
        }
        Ok(())
    }

    /// Background packet pump to refill `held` after a warm take (cross-GOP).
    ///
    /// Early `take_held` used to return with no feeder — Play then drained held
    /// and underran at the next GOP. **Retarget** a live feeder when possible;
    /// only bump `pump_epoch` when the previous pump is dead.
    /// mediabunny: `src/media-sink.ts` `mediaSamplesInRange` — one packet loop
    /// while the consumer pulls (no respawn per sample).
    fn ensure_warm_pump(
        &self,
        track: Rc<Mp4VideoTrack>,
        key_do: usize,
        target_do: usize,
        target_us: i64,
    ) {
        self.retarget_play_cursor(target_do, target_us);
        if self.pump_is_live() {
            return;
        }
        self.pump_epoch.set(self.pump_epoch.get().wrapping_add(1));
        let pump_epoch_now = self.pump_epoch.get();
        let signals = Rc::new(PumpSignals::default());
        *self.live_pump.borrow_mut() = Some(LivePump {
            signals: Rc::clone(&signals),
            epoch: pump_epoch_now,
        });
        spawn_local(
            FramePump {
                core: Rc::clone(&self.core),
                pump_epoch: Rc::clone(&self.pump_epoch),
                epoch: pump_epoch_now,
                track,
                key_do,
                cursor: Rc::clone(&self.play_cursor),
                signals,
                warm_only: true,
            }
            .run(),
        );
    }

    fn retarget_play_cursor(&self, target_do: usize, target_us: i64) {
        self.play_cursor.target_do.set(target_do);
        self.play_cursor.target_us.set(target_us);
    }

    fn pump_is_live(&self) -> bool {
        let epoch = self.pump_epoch.get();
        self.live_pump
            .borrow()
            .as_ref()
            .is_some_and(|p| p.epoch == epoch && !p.signals.stop.get() && !p.signals.done.get())
    }

    /// Decode every presentation frame in the GOP starting at sync `key_frame`.
    ///
    /// Delivers remapped frames to `on_frame` as they emerge so `held` stays
    /// empty (mediabunny drains `sampleQueue` as the consumer pulls) and the
    /// dense-cache strip can paint each tick without waiting for the whole GOP.
    /// Parking the whole GOP in `held` forced `MAX_DECODE_QUEUE_WITH_HELD` (8)
    /// and wedged at `q≈9, held=16` → backpressure timeout.
    ///
    /// Stays a **single** task: `flush_held_into_gop` drains `held` on every
    /// backpressure turn, so the consumer-take wake the dual-async
    /// [`Self::decode_frame`] path needs has no equivalent here.
    ///
    /// On cancel: hard-reset. Frames already delivered to `on_frame` stay with
    /// the consumer (partial densify is OK; holes re-arm fill).
    pub async fn decode_gop(
        &mut self,
        track: &Rc<Mp4VideoTrack>,
        key_frame: FrameId,
        is_cancelled: &dyn Fn() -> bool,
        on_frame: &mut dyn FnMut(FrameId, OwnedVideoFrame),
    ) -> Result<(), MediaError> {
        if is_cancelled() {
            return self.abort_gop_partial();
        }
        let core = Rc::clone(&self.core);
        // Densify may start before bg verify finishes — verify this key on demand.
        let key_frame = track.ensure_key_verified(key_frame).await?;
        if is_cancelled() {
            return self.abort_gop_partial();
        }
        let (key_do, gop_ids) = {
            let index = track.index();
            let key_entry = index
                .get(key_frame)
                .ok_or(MediaError::FrameOutOfRange(key_frame, track.frame_count()))?;
            if !key_entry.is_sync {
                return Err(MediaError::WebCodecs(format!(
                    "decode_gop requires sync key (frame={key_frame})"
                )));
            }
            let key_do = key_entry.decode_order;
            let gop_ids = index
                .gop_frame_ids(key_frame)
                .ok_or(MediaError::FrameOutOfRange(key_frame, track.frame_count()))?;
            (key_do, gop_ids)
        };
        let mut pts_to_id: HashMap<i64, FrameId> = HashMap::with_capacity(gop_ids.len());
        {
            let index = track.index();
            for id in &gop_ids {
                let e = index
                    .get(*id)
                    .ok_or(MediaError::FrameOutOfRange(*id, track.frame_count()))?;
                pts_to_id.insert(
                    ticks_to_micros(e.composition_timestamp_ticks, e.timescale),
                    *id,
                );
            }
        }

        core.borrow_mut().begin_key_cascade(key_do, false, false)?;

        let key_sample = track.encoded_sample(key_frame).await?;
        if is_cancelled() {
            return self.abort_gop_partial();
        }
        match feed_sample_gop(
            &core,
            &key_sample,
            true,
            &mut pts_to_id,
            on_frame,
            is_cancelled,
        )
        .await
        {
            Ok(()) => {}
            Err(MediaError::Cancelled) => return self.abort_gop_partial(),
            Err(e) => return Err(e),
        }
        await_outputs_if_pending(&core).await?;
        {
            let mut c = core.borrow_mut();
            c.collect_outputs_into_held()?;
            c.flush_held_into_gop(on_frame, &mut pts_to_id);
        }
        // Macrotask so paused rAF can refresh the ready strip mid-GOP.
        let _ = JsFuture::from(sleep_promise_ms(0)).await;

        let max_feeds = gop_ids.len().saturating_add(8).max(16);
        for _ in 0..max_feeds {
            if is_cancelled() {
                return self.abort_gop_partial();
            }
            let fed = core.borrow().fed_through_order.unwrap_or(key_do);
            let next = track.samples_after_in_gop(key_do, fed, 1).await?;
            if next.is_empty() {
                break;
            }
            match feed_sample_gop(
                &core,
                &next[0],
                false,
                &mut pts_to_id,
                on_frame,
                is_cancelled,
            )
            .await
            {
                Ok(()) => {}
                Err(MediaError::Cancelled) => return self.abort_gop_partial(),
                Err(e) => return Err(e),
            }
            await_outputs_if_pending(&core).await?;
            {
                let mut c = core.borrow_mut();
                c.collect_outputs_into_held()?;
                c.flush_held_into_gop(on_frame, &mut pts_to_id);
            }
            // Macrotask yield — microtask-only spins starve rAF / strip updates.
            let _ = JsFuture::from(sleep_promise_ms(0)).await;
        }

        for _ in 0..32 {
            if is_cancelled() {
                return self.abort_gop_partial();
            }
            await_outputs_if_pending(&core).await?;
            let idle = {
                let mut c = core.borrow_mut();
                c.collect_outputs_into_held()?;
                c.flush_held_into_gop(on_frame, &mut pts_to_id);
                c.decode_queue_size() == 0 && !c.raw_outputs_pending()
            };
            if idle {
                break;
            }
            let _ = JsFuture::from(sleep_promise_ms(0)).await;
        }
        let pending = {
            let c = core.borrow();
            !c.input_timestamps.is_empty() || c.decode_queue_size() > 0
        };
        if pending {
            // Settle abort must not wait out a 2s flush while exact owns HW.
            if is_cancelled() {
                return self.abort_gop_partial();
            }
            let _ = flush_with_timeout(&core).await;
            let mut c = core.borrow_mut();
            c.collect_outputs_into_held()?;
            c.flush_held_into_gop(on_frame, &mut pts_to_id);
        }

        let mut c = core.borrow_mut();
        c.fed_through_order = None;
        c.key_order = None;
        c.input_timestamps.clear();
        Ok(())
    }

    /// Cancel path: hard-reset so in-flight browser outputs cannot desync PTS remap.
    fn abort_gop_partial(&mut self) -> Result<(), MediaError> {
        let _ = self.reset();
        Err(MediaError::Cancelled)
    }
}

impl Drop for WebCodecsDecoder {
    fn drop(&mut self) {
        // Stop any pump that still holds an `Rc` to the core, then tear the
        // browser decoder down even if that pump keeps the core alive a turn.
        self.pump_epoch.set(self.pump_epoch.get().wrapping_add(1));
        if let Ok(mut core) = self.core.try_borrow_mut() {
            core.detach_decoder();
        }
    }
}
