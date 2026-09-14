//! WebCodecs `VideoDecoder` wrapper.
//!
//! Playback follows mediabunny's model: feed packets in **decode order**, park
//! outputs in a presentation buffer with remapped PTS, and pull the target
//! frame when ready. Prefer feeding further ahead in the GOP over `flush()`.
//! Apply `decodeQueueSize` backpressure before feeding more packets.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use js_sys::Uint8Array;
use wasm_bindgen::closure::Closure;
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;
use web_sys::{
    EncodedVideoChunk, EncodedVideoChunkInit, EncodedVideoChunkType, VideoDecoder,
    VideoDecoderConfig, VideoDecoderInit, VideoFrame,
};

use crate::bitstream::{avc_nal_length_size, sanitize_avc_packet_for_chromium};
use crate::decode::owned_video_frame::OwnedVideoFrame;
use crate::demux::Mp4VideoTrack;
use crate::error::MediaError;
use crate::types::{EncodedSample, FrameId, VideoMetadata};

/// Max in-flight encoded chunks when decoded frames are already buffered
/// (mediabunny `computeMaxQueueSize` when `decodedSampleQueueSize > 0`).
const MAX_DECODE_QUEUE_WITH_HELD: u32 = 8;
/// Cap when the held/output buffers are empty — some browsers need a deeper
/// encoded queue before emitting reordered frames.
const MAX_DECODE_QUEUE_EMPTY: u32 = 40;
/// While key→N prune closes every output before the target (`held` stays empty),
/// keep the encoded queue shallow so we pace on close instead of piling
/// `decodeQ≈10` with nothing retained (VideoFrame pool / densify starvation).
const SEEK_PRUNE_MAX_QUEUE: u32 = 4;

fn compute_max_queue_size(decoded_sample_queue_size: u32) -> u32 {
    // mediabunny `computeMaxQueueSize`
    if decoded_sample_queue_size == 0 {
        MAX_DECODE_QUEUE_EMPTY
    } else {
        MAX_DECODE_QUEUE_WITH_HELD
    }
}

#[derive(Clone)]
struct DecoderConfig {
    codec: String,
    width: u32,
    height: u32,
    description: Vec<u8>,
}

impl DecoderConfig {
    fn from_meta(meta: &VideoMetadata) -> Result<Self, MediaError> {
        Ok(Self {
            codec: meta
                .codec
                .codec_string
                .clone()
                .ok_or(MediaError::UnsupportedCodec)?,
            width: meta.width,
            height: meta.height,
            description: meta.codec.description.clone(),
        })
    }

    /// Build a WebCodecs config.
    ///
    /// Do **not** set `hardwareAcceleration: prefer-hardware` on the support
    /// check — many browsers report `supported: false` even when software
    /// (or opportunistic HW) decode works, which blocks open entirely.
    fn to_web_config(&self, optimize_for_latency: bool) -> VideoDecoderConfig {
        let config = VideoDecoderConfig::new(&self.codec);
        config.set_coded_width(self.width);
        config.set_coded_height(self.height);
        config.set_description(&Uint8Array::from(self.description.as_slice()));
        // Prefer low-latency configure for reliable open / first-frame seek.
        // (Throughput-oriented `false` can be revisited once open is stable.)
        config.set_optimize_for_latency(optimize_for_latency);
        config
    }
}

struct HeldFrame {
    pts_us: i64,
    frame: OwnedVideoFrame,
}

/// Decode presentation frames via WebCodecs with a warm decode-order pump.
pub struct WebCodecsDecoder {
    decoder: VideoDecoder,
    config: DecoderConfig,
    optimize_for_latency: bool,
    /// Bumped on teardown so late browser callbacks become no-ops.
    generation: Rc<Cell<u64>>,
    frames: Rc<RefCell<Vec<OwnedVideoFrame>>>,
    error: Rc<RefCell<Option<String>>>,
    /// Resolvers waiting for the next `output` callback (mediabunny-style).
    output_waiters: Rc<RefCell<Vec<js_sys::Function>>>,
    /// Resolvers waiting for `dequeue` (decodeQueueSize decreased).
    dequeue_waiters: Rc<RefCell<Vec<js_sys::Function>>>,
    fed_through_order: Option<usize>,
    key_order: Option<usize>,
    /// Sorted PTS (µs) of fed chunks — shifted onto outputs in presentation order.
    input_timestamps: Vec<i64>,
    held: Vec<HeldFrame>,
    /// Packets fed since last `configure` (mediabunny `currentPacketIndex`).
    packets_fed: usize,
    /// Optional: salvage pruned early frames during key→N into the host dense cache.
    prune_interim: RefCell<Option<PruneInterim>>,
    _on_output: Closure<dyn FnMut(JsValue)>,
    _on_error: Closure<dyn FnMut(JsValue)>,
    _on_dequeue: Closure<dyn FnMut(JsValue)>,
}

/// Hands pruned PTS `< target` frames to the host instead of closing them.
///
/// Settle/seek installs this so one key→N cascade both yields exact **and** warms
/// dense (densify after exact becomes mostly a no-op). Host must **not** blit —
/// only `insert_frame` (blit looked like play from GOP start).
struct PruneInterim {
    pts_to_id: HashMap<i64, FrameId>,
    on_frame: Box<dyn FnMut(FrameId, OwnedVideoFrame)>,
}

/// Max delta packets to feed after the target's decode_order to coax B-frame emit
/// before flushing (mediabunny feeds ahead; whole-GOP feed is unnecessary).
const REORDER_FEED_SLACK: usize = 24;

impl WebCodecsDecoder {
    pub async fn create(meta: &VideoMetadata) -> Result<Self, MediaError> {
        let config = DecoderConfig::from_meta(meta)?;
        // Keep the previously working open path (`optimizeForLatency: true`).
        // Throughput tweaks must not block `isConfigSupported` / first seek.
        let optimize_for_latency = true;
        let web_config = config.to_web_config(optimize_for_latency);
        let support = JsFuture::from(VideoDecoder::is_config_supported(&web_config))
            .await
            .map_err(MediaError::from)?;
        let supported = js_sys::Reflect::get(&support, &"supported".into())
            .ok()
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if !supported {
            return Err(MediaError::CodecUnsupported(config.codec.clone()));
        }
        Self::from_config(config, optimize_for_latency)
    }

    fn from_config(config: DecoderConfig, optimize_for_latency: bool) -> Result<Self, MediaError> {
        let web_config = config.to_web_config(optimize_for_latency);

        let generation = Rc::new(Cell::new(0));
        let frames: Rc<RefCell<Vec<OwnedVideoFrame>>> = Rc::new(RefCell::new(Vec::new()));
        let error: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));
        let output_waiters: Rc<RefCell<Vec<js_sys::Function>>> = Rc::new(RefCell::new(Vec::new()));
        let dequeue_waiters: Rc<RefCell<Vec<js_sys::Function>>> = Rc::new(RefCell::new(Vec::new()));

        let gen_cb = Rc::clone(&generation);
        let my_gen = gen_cb.get();
        let frames_cb = Rc::clone(&frames);
        let waiters_cb = Rc::clone(&output_waiters);
        let dequeue_on_output = Rc::clone(&dequeue_waiters);
        let on_output = Closure::wrap(Box::new(move |value: JsValue| {
            if gen_cb.get() != my_gen {
                if let Ok(frame) = value.dyn_into::<VideoFrame>() {
                    drop(OwnedVideoFrame::new(frame));
                }
                return;
            }
            if let Ok(frame) = value.dyn_into::<VideoFrame>() {
                frames_cb.borrow_mut().push(OwnedVideoFrame::new(frame));
            }
            let waiting = std::mem::take(&mut *waiters_cb.borrow_mut());
            // Defer resolves — sync wake from inside this Closure re-enters Rust
            // (decode → recreate → drop Closure) and panics with
            // "closure invoked recursively or after being dropped".
            wake_waiters_deferred(waiting);
            // mediabunny: `onQueueDequeue()` on every decoder output (even when
            // the sample will be closed) so the encode pump unblocks.
            let waiting = std::mem::take(&mut *dequeue_on_output.borrow_mut());
            wake_waiters_deferred(waiting);
        }) as Box<dyn FnMut(JsValue)>);

        let gen_err = Rc::clone(&generation);
        let my_gen_err = gen_err.get();
        let error_cb = Rc::clone(&error);
        let on_error = Closure::wrap(Box::new(move |value: JsValue| {
            if gen_err.get() != my_gen_err {
                return;
            }
            let msg = value
                .as_string()
                .or_else(|| {
                    js_sys::Reflect::get(&value, &"message".into())
                        .ok()
                        .and_then(|v| v.as_string())
                })
                .unwrap_or_else(|| format!("{value:?}"));
            *error_cb.borrow_mut() = Some(msg);
        }) as Box<dyn FnMut(JsValue)>);

        let gen_dq = Rc::clone(&generation);
        let my_gen_dq = gen_dq.get();
        let dequeue_cb = Rc::clone(&dequeue_waiters);
        let on_dequeue = Closure::wrap(Box::new(move |_value: JsValue| {
            if gen_dq.get() != my_gen_dq {
                return;
            }
            let waiting = std::mem::take(&mut *dequeue_cb.borrow_mut());
            wake_waiters_deferred(waiting);
        }) as Box<dyn FnMut(JsValue)>);

        let init = VideoDecoderInit::new(
            on_error.as_ref().unchecked_ref(),
            on_output.as_ref().unchecked_ref(),
        );
        let decoder = VideoDecoder::new(&init).map_err(MediaError::from)?;
        decoder.set_ondequeue(Some(on_dequeue.as_ref().unchecked_ref()));
        decoder.configure(&web_config).map_err(MediaError::from)?;

        Ok(Self {
            decoder,
            config,
            optimize_for_latency,
            generation,
            frames,
            error,
            output_waiters,
            dequeue_waiters,
            fed_through_order: None,
            key_order: None,
            input_timestamps: Vec::new(),
            held: Vec::new(),
            packets_fed: 0,
            prune_interim: RefCell::new(None),
            _on_output: on_output,
            _on_error: on_error,
            _on_dequeue: on_dequeue,
        })
    }

    /// Close the browser decoder and retire callbacks without dropping Closures
    /// that JS may still invoke (would panic inside wasm-bindgen).
    fn detach_decoder(&mut self) {
        self.generation.set(self.generation.get().wrapping_add(1));
        let _ = self.decoder.set_ondequeue(None);
        let _ = self.decoder.close();
        for frame in self.frames.borrow_mut().drain(..) {
            drop(frame);
        }
        self.clear_held();
        self.input_timestamps.clear();
        self.packets_fed = 0;
        let waiting = std::mem::take(&mut *self.output_waiters.borrow_mut());
        wake_waiters_deferred(waiting);
        let waiting = std::mem::take(&mut *self.dequeue_waiters.borrow_mut());
        wake_waiters_deferred(waiting);
        *self.error.borrow_mut() = None;
    }

    fn recreate(&mut self) -> Result<(), MediaError> {
        // Keep dense-salvage sink across hard reset (All-Intra / wedge recovery).
        let interim = self.prune_interim.borrow_mut().take();
        self.detach_decoder();
        let config = self.config.clone();
        let optimize_for_latency = self.optimize_for_latency;
        // Retire Closures before `*self = …` Drop — JS may still invoke them.
        let old_output = std::mem::replace(&mut self._on_output, Closure::new(|_| {}));
        let old_error = std::mem::replace(&mut self._on_error, Closure::new(|_| {}));
        let old_dequeue = std::mem::replace(&mut self._on_dequeue, Closure::new(|_| {}));
        std::mem::forget(old_output);
        std::mem::forget(old_error);
        std::mem::forget(old_dequeue);

        *self = Self::from_config(config, optimize_for_latency)?;
        *self.prune_interim.borrow_mut() = interim;
        Ok(())
    }

    pub fn invalidate(&mut self) {
        // Prefer a hard decoder reset — soft clear of timestamps/held while the
        // browser still has in-flight chunks desyncs PTS remap.
        let _ = self.reset();
    }

    /// Close and recreate the VideoDecoder; clear warm GOP / held / timestamps.
    pub fn reset(&mut self) -> Result<(), MediaError> {
        self.recreate()
    }

    /// Start a new key→… cascade.
    ///
    /// Soft-clear only when the browser queue is empty **and** no PTS remap is
    /// pending. Otherwise hard-`recreate` — soft-switching over in-flight HW
    /// output desyncs timestamps (All-Intra scrub → wrong frame / hung decode /
    /// `decoder busy` while flush holds the slot).
    fn begin_key_cascade(&mut self, key_do: usize, force_hard: bool) -> Result<(), MediaError> {
        // Pull any already-delivered outputs before deciding idle vs hard reset.
        let _ = self.collect_outputs_into_held();
        self.clear_held();
        for frame in self.frames.borrow_mut().drain(..) {
            drop(frame);
        }

        let pending_remap = !self.input_timestamps.is_empty();
        let busy = self.decode_queue_size() > 0 || pending_remap;
        if force_hard || busy {
            self.recreate()?;
        } else {
            self.input_timestamps.clear();
            self.fed_through_order = None;
            self.key_order = None;
        }
        self.key_order = Some(key_do);
        Ok(())
    }

    fn clear_held(&mut self) {
        self.held.clear();
    }

    /// mediabunny `onQueueDequeue` after consumer take / early sample close.
    fn wake_dequeue_waiters(&self) {
        let waiting = std::mem::take(&mut *self.dequeue_waiters.borrow_mut());
        wake_waiters_deferred(waiting);
    }

    fn push_input_timestamp(&mut self, pts_us: i64) {
        let i = self.input_timestamps.partition_point(|&t| t < pts_us);
        self.input_timestamps.insert(i, pts_us);
    }

    fn collect_outputs_into_held(&mut self) -> Result<(), MediaError> {
        for frame in self.frames.borrow_mut().drain(..) {
            let Some(pts_us) = self.input_timestamps.first().copied() else {
                // Late output after flush/cancel/recreate race — drop; do not
                // poison the session (was: "decoded frame without queued input timestamp").
                drop(frame);
                continue;
            };
            self.input_timestamps.remove(0);
            self.held.push(HeldFrame { pts_us, frame });
        }
        Ok(())
    }

    fn take_held(&mut self, target_us: i64) -> Option<OwnedVideoFrame> {
        let idx = self.held.iter().position(|h| h.pts_us == target_us)?;
        let frame = self.held.remove(idx).frame;
        // Consumer took a sample — unblock encode pump (mediabunny `onQueueDequeue`).
        self.wake_dequeue_waiters();
        Some(frame)
    }

    fn drop_held_before(&mut self, target_us: i64) {
        let mut dropped = false;
        let mut i = 0;
        while i < self.held.len() {
            if self.held[i].pts_us < target_us {
                let h = self.held.swap_remove(i);
                self.deliver_or_drop_pruned(h.pts_us, h.frame);
                dropped = true;
            } else {
                i += 1;
            }
        }
        if dropped {
            self.wake_dequeue_waiters();
        }
    }

    /// Remap outputs and close anything strictly before the seek target in the
    /// same pass (mediabunny closes early samples in the output callback before
    /// they enter `sampleQueue`). Park-then-prune a turn later left `decodeQ`
    /// elevated with `held=0` mid key→N and starved the VideoFrame pool.
    ///
    /// When [`Self::prune_interim`] is set, pruned frames go to the host dense
    /// cache instead of close — one cascade warms cache + yields exact.
    fn collect_and_prune_held(&mut self, target_us: i64) -> Result<(), MediaError> {
        let mut dropped = false;
        for frame in self.frames.borrow_mut().drain(..) {
            let Some(pts_us) = self.input_timestamps.first().copied() else {
                drop(frame);
                continue;
            };
            self.input_timestamps.remove(0);
            if pts_us < target_us {
                self.deliver_or_drop_pruned(pts_us, frame);
                dropped = true;
            } else {
                self.held.push(HeldFrame { pts_us, frame });
            }
        }
        self.drop_held_before(target_us);
        if dropped {
            self.wake_dequeue_waiters();
        }
        Ok(())
    }

    fn deliver_or_drop_pruned(&self, pts_us: i64, frame: OwnedVideoFrame) {
        let mut slot = self.prune_interim.borrow_mut();
        if let Some(interim) = slot.as_mut() {
            if let Some(id) = interim.pts_to_id.remove(&pts_us) {
                (interim.on_frame)(id, frame);
                return;
            }
        }
        drop(frame);
    }

    fn decode_queue_size(&self) -> u32 {
        self.decoder.decode_queue_size()
    }

    /// In-flight encoded chunks waiting in the browser decoder.
    pub fn browser_decode_queue_size(&self) -> u32 {
        self.decode_queue_size()
    }

    /// Decoded frames parked for presentation-order pull (B-frame reorder).
    pub fn held_len(&self) -> usize {
        self.held.len()
    }

    /// Decode until `frame` is available, feeding decode-order packets as needed.
    ///
    /// `is_cancelled` — host op epoch (rapid PREV/seek); bail with [`MediaError::Cancelled`].
    pub async fn decode_frame(
        &mut self,
        track: &Mp4VideoTrack,
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
        track: &Mp4VideoTrack,
        frame: FrameId,
        is_cancelled: &dyn Fn() -> bool,
        on_interim: Box<dyn FnMut(FrameId, OwnedVideoFrame)>,
    ) -> Result<OwnedVideoFrame, MediaError> {
        let key_id = track
            .index()
            .nearest_keyframe(frame)
            .ok_or(MediaError::FrameOutOfRange(frame, track.frame_count()))?;
        let gop_ids = track.index().gop_frame_ids(key_id).unwrap_or_default();
        let mut pts_to_id: HashMap<i64, FrameId> = HashMap::with_capacity(gop_ids.len());
        for id in &gop_ids {
            let e = track
                .index()
                .get(*id)
                .ok_or(MediaError::FrameOutOfRange(*id, track.frame_count()))?;
            pts_to_id.insert(
                ticks_to_micros(e.composition_timestamp_ticks, e.timescale),
                *id,
            );
        }
        *self.prune_interim.borrow_mut() = Some(PruneInterim {
            pts_to_id,
            on_frame: on_interim,
        });
        let result = self.decode_frame_inner(track, frame, is_cancelled).await;
        *self.prune_interim.borrow_mut() = None;
        result
    }

    async fn decode_frame_inner(
        &mut self,
        track: &Mp4VideoTrack,
        frame: FrameId,
        is_cancelled: &dyn Fn() -> bool,
    ) -> Result<OwnedVideoFrame, MediaError> {
        if is_cancelled() {
            return Err(MediaError::Cancelled);
        }
        let entry = track
            .index()
            .get(frame)
            .ok_or(MediaError::FrameOutOfRange(frame, track.frame_count()))?;
        let target_us = ticks_to_micros(entry.composition_timestamp_ticks, entry.timescale);
        let target_do = entry.decode_order;
        let key_id = track
            .index()
            .nearest_keyframe(frame)
            .ok_or(MediaError::FrameOutOfRange(frame, track.frame_count()))?;
        let key_do = track
            .index()
            .get(key_id)
            .map(|e| e.decode_order)
            .ok_or(MediaError::FrameOutOfRange(key_id, track.frame_count()))?;

        self.collect_and_prune_held(target_us)?;
        if let Some(ready) = self.take_held(target_us) {
            return Ok(ready);
        }

        // Restart when the GOP changes, or when the target was already emitted and
        // discarded (e.g. presentation queue cleared after prefetch) — WebCodecs
        // cannot re-emit a consumed frame without cascading from the key again.
        let target_consumed = self.fed_through_order.is_some_and(|fed| {
            entry.decode_order <= fed && !self.input_timestamps.contains(&target_us)
        });
        if self.key_order != Some(key_do) || target_consumed {
            // Soft key switch when idle — All-Intra hits a new sync every scrub step.
            // Hard recreate only if the previous cascade left in-flight work / held.
            self.begin_key_cascade(key_do, target_consumed)?;
            let key_sample = track.encoded_sample(key_id).await?;
            if is_cancelled() {
                return Err(MediaError::Cancelled);
            }
            self.feed_sample(&key_sample, true, target_us, is_cancelled)
                .await?;
            self.fed_through_order = Some(key_do);
            // Poll with macrotasks (see `poll_output_or_macrotask`) — microtask-only
            // never saw HW output on Wing It and forced flush.
            for _ in 0..32 {
                if is_cancelled() {
                    return Err(MediaError::Cancelled);
                }
                self.await_outputs_if_pending().await?;
                self.collect_and_prune_held(target_us)?;
                if let Some(ready) = self.take_held(target_us) {
                    return Ok(ready);
                }
                // Only stop early when nothing is in-flight — empty queue alone can
                // race the output callback (All-Intra then fell into 2s flush).
                if self.decode_queue_size() == 0
                    && self.frames.borrow().is_empty()
                    && self.input_timestamps.is_empty()
                {
                    break;
                }
            }
            // Prefer feeding one more GOP packet over flush — flush forces the
            // next decode() to be a key (Chromium: "needs a key chunk").
            // All-Intra: GOP is one sample — coax is empty; hard-retry instead of
            // a multi-second flush that pins the decoder slot (→ `decoder busy`).
            if key_id == frame {
                let coax = track.samples_after_in_gop(key_do, key_do, 1).await?;
                if let Some(sample) = coax.first() {
                    self.feed_sample(sample, false, target_us, is_cancelled)
                        .await?;
                    self.fed_through_order = Some(sample.decode_order);
                    for _ in 0..32 {
                        if is_cancelled() {
                            return Err(MediaError::Cancelled);
                        }
                        self.await_outputs_if_pending().await?;
                        self.collect_and_prune_held(target_us)?;
                        if let Some(ready) = self.take_held(target_us) {
                            return Ok(ready);
                        }
                        if self.decode_queue_size() == 0
                            && self.frames.borrow().is_empty()
                            && self.input_timestamps.is_empty()
                        {
                            break;
                        }
                    }
                } else if self.held.iter().all(|h| h.pts_us != target_us) {
                    // Sole sync (All-Intra): one hard reset + re-feed, no long flush.
                    if is_cancelled() {
                        return Err(MediaError::Cancelled);
                    }
                    self.recreate()?;
                    self.key_order = Some(key_do);
                    let key_sample = track.encoded_sample(key_id).await?;
                    if is_cancelled() {
                        return Err(MediaError::Cancelled);
                    }
                    self.feed_sample(&key_sample, true, target_us, is_cancelled)
                        .await?;
                    self.fed_through_order = Some(key_do);
                    for _ in 0..48 {
                        if is_cancelled() {
                            return Err(MediaError::Cancelled);
                        }
                        self.await_outputs_if_pending().await?;
                        self.collect_and_prune_held(target_us)?;
                        if let Some(ready) = self.take_held(target_us) {
                            return Ok(ready);
                        }
                        if self.decode_queue_size() == 0
                            && self.frames.borrow().is_empty()
                            && self.input_timestamps.is_empty()
                        {
                            break;
                        }
                    }
                }
                if self.held.iter().all(|h| h.pts_us != target_us) {
                    // Last resort — short flush (not 2s) then clear warm state.
                    if is_cancelled() {
                        return Err(MediaError::Cancelled);
                    }
                    let _ = self.flush_with_timeout_ms(250).await;
                    self.collect_and_prune_held(target_us)?;
                    self.fed_through_order = None;
                    self.key_order = None;
                    self.input_timestamps.clear();
                    if let Some(ready) = self.take_held(target_us) {
                        return Ok(ready);
                    }
                }
            }
            self.collect_and_prune_held(target_us)?;
            if let Some(ready) = self.take_held(target_us) {
                return Ok(ready);
            }
        }

        // Pump decode-order samples until the target PTS lands in `held`.
        // Feeding a little past the target (B/P reorder) is intentional; do not
        // drain the entire GOP — that amplified PTS desync on rapid PREV.
        let max_feeds = (track.frame_count() as usize).saturating_add(32).max(64);
        for _ in 0..max_feeds {
            if is_cancelled() {
                return Err(MediaError::Cancelled);
            }
            self.check_error()?;
            self.collect_and_prune_held(target_us)?;
            if let Some(ready) = self.take_held(target_us) {
                return Ok(ready);
            }

            let fed = self.fed_through_order.unwrap_or(key_do);
            let past_target = fed >= target_do;
            let force_flush = past_target && fed.saturating_sub(target_do) >= REORDER_FEED_SLACK;
            let next = if force_flush {
                Vec::new()
            } else {
                track.samples_after_in_gop(key_do, fed, 1).await?
            };
            if next.is_empty() {
                if let Some(ready) = self.finish_gop_for_target(target_us, is_cancelled).await? {
                    return Ok(ready);
                }
                return Err(MediaError::WebCodecs(format!(
                    "flush/idle without pts={target_us}µs (held={})",
                    self.held.len()
                )));
            }

            let sample = &next[0];
            self.feed_sample(sample, false, target_us, is_cancelled)
                .await?;
            self.fed_through_order = Some(sample.decode_order);
            // If the decoder is idle with no output, it is waiting for more input
            // (typical B-frame reorder) — do not block; feed the next packet.
            self.await_outputs_if_pending().await?;
        }

        Err(MediaError::WebCodecs(format!(
            "decode pump gave up waiting for pts={target_us}µs"
        )))
    }

    /// Decode every presentation frame in the GOP starting at sync `key_frame`.
    ///
    /// Delivers remapped frames to `on_frame` as they emerge so `held` stays
    /// empty (mediabunny drains `sampleQueue` as the consumer pulls) and the
    /// dense-cache strip can paint each tick without waiting for the whole GOP.
    /// Parking the whole GOP in `held` forced `MAX_DECODE_QUEUE_WITH_HELD` (8)
    /// and wedged at `q≈9, held=16` → backpressure timeout.
    ///
    /// On cancel: hard-reset. Frames already delivered to `on_frame` stay with
    /// the consumer (partial densify is OK; holes re-arm fill).
    pub async fn decode_gop(
        &mut self,
        track: &Mp4VideoTrack,
        key_frame: FrameId,
        is_cancelled: &dyn Fn() -> bool,
        on_frame: &mut dyn FnMut(FrameId, OwnedVideoFrame),
    ) -> Result<(), MediaError> {
        if is_cancelled() {
            return self.abort_gop_partial();
        }
        let key_entry = track
            .index()
            .get(key_frame)
            .ok_or(MediaError::FrameOutOfRange(key_frame, track.frame_count()))?;
        if !key_entry.is_sync {
            return Err(MediaError::WebCodecs(format!(
                "decode_gop requires sync key (frame={key_frame})"
            )));
        }
        let key_do = key_entry.decode_order;
        let gop_ids = track
            .index()
            .gop_frame_ids(key_frame)
            .ok_or(MediaError::FrameOutOfRange(key_frame, track.frame_count()))?;
        let mut pts_to_id: HashMap<i64, FrameId> = HashMap::with_capacity(gop_ids.len());
        for id in &gop_ids {
            let e = track
                .index()
                .get(*id)
                .ok_or(MediaError::FrameOutOfRange(*id, track.frame_count()))?;
            pts_to_id.insert(
                ticks_to_micros(e.composition_timestamp_ticks, e.timescale),
                *id,
            );
        }

        self.begin_key_cascade(key_do, false)?;

        let key_sample = track.encoded_sample(key_frame).await?;
        if is_cancelled() {
            return self.abort_gop_partial();
        }
        match self
            .feed_sample_gop(&key_sample, true, &mut pts_to_id, on_frame, is_cancelled)
            .await
        {
            Ok(()) => {}
            Err(MediaError::Cancelled) => return self.abort_gop_partial(),
            Err(e) => return Err(e),
        }
        self.fed_through_order = Some(key_do);
        self.await_outputs_if_pending().await?;
        self.collect_outputs_into_held()?;
        self.flush_held_into_gop(on_frame, &mut pts_to_id);
        // Macrotask so paused rAF can refresh the ready strip mid-GOP.
        let _ = JsFuture::from(sleep_promise_ms(0)).await;

        let max_feeds = gop_ids.len().saturating_add(8).max(16);
        for _ in 0..max_feeds {
            if is_cancelled() {
                return self.abort_gop_partial();
            }
            let fed = self.fed_through_order.unwrap_or(key_do);
            let next = track.samples_after_in_gop(key_do, fed, 1).await?;
            if next.is_empty() {
                break;
            }
            match self
                .feed_sample_gop(&next[0], false, &mut pts_to_id, on_frame, is_cancelled)
                .await
            {
                Ok(()) => {}
                Err(MediaError::Cancelled) => return self.abort_gop_partial(),
                Err(e) => return Err(e),
            }
            self.fed_through_order = Some(next[0].decode_order);
            self.await_outputs_if_pending().await?;
            self.collect_outputs_into_held()?;
            self.flush_held_into_gop(on_frame, &mut pts_to_id);
            // Macrotask yield — microtask-only spins starve rAF / strip updates.
            let _ = JsFuture::from(sleep_promise_ms(0)).await;
        }

        for _ in 0..32 {
            if is_cancelled() {
                return self.abort_gop_partial();
            }
            self.await_outputs_if_pending().await?;
            self.collect_outputs_into_held()?;
            self.flush_held_into_gop(on_frame, &mut pts_to_id);
            if self.decode_queue_size() == 0 && self.frames.borrow().is_empty() {
                break;
            }
            let _ = JsFuture::from(sleep_promise_ms(0)).await;
        }
        if !self.input_timestamps.is_empty() || self.decode_queue_size() > 0 {
            // Settle abort must not wait out a 2s flush while exact owns HW.
            if is_cancelled() {
                return self.abort_gop_partial();
            }
            let _ = self.flush_with_timeout().await;
            self.collect_outputs_into_held()?;
            self.flush_held_into_gop(on_frame, &mut pts_to_id);
        }

        self.fed_through_order = None;
        self.key_order = None;
        self.input_timestamps.clear();
        Ok(())
    }

    /// Move remapped `held` frames to the consumer so `held` stays empty
    /// for backpressure (max queue 40 vs 8).
    fn flush_held_into_gop(
        &mut self,
        on_frame: &mut dyn FnMut(FrameId, OwnedVideoFrame),
        pts_to_id: &mut HashMap<i64, FrameId>,
    ) {
        if self.held.is_empty() {
            return;
        }
        for h in self.held.drain(..) {
            if let Some(id) = pts_to_id.remove(&h.pts_us) {
                on_frame(id, h.frame);
            } else {
                drop(h.frame);
            }
        }
        // Consumer drained sampleQueue — mediabunny `onQueueDequeue`.
        self.wake_dequeue_waiters();
    }

    /// Cancel path: hard-reset so in-flight browser outputs cannot desync PTS remap.
    fn abort_gop_partial(&mut self) -> Result<(), MediaError> {
        let _ = self.reset();
        Err(MediaError::Cancelled)
    }

    /// End-of-input / reorder-slack: drain outputs, then flush once.
    async fn finish_gop_for_target(
        &mut self,
        target_us: i64,
        is_cancelled: &dyn Fn() -> bool,
    ) -> Result<Option<OwnedVideoFrame>, MediaError> {
        for _ in 0..32 {
            if is_cancelled() {
                return Err(MediaError::Cancelled);
            }
            self.await_outputs_if_pending().await?;
            self.collect_and_prune_held(target_us)?;
            if let Some(ready) = self.take_held(target_us) {
                return Ok(Some(ready));
            }
            if self.decode_queue_size() == 0
                && self.frames.borrow().is_empty()
                && self.input_timestamps.is_empty()
            {
                break;
            }
            if self.decode_queue_size() == 0 && self.frames.borrow().is_empty() {
                break;
            }
        }
        self.collect_and_prune_held(target_us)?;
        if let Some(ready) = self.take_held(target_us) {
            return Ok(Some(ready));
        }
        if !self.input_timestamps.is_empty() || self.decode_queue_size() > 0 {
            self.flush_with_timeout().await?;
            self.check_error()?;
            self.collect_and_prune_held(target_us)?;
            if let Some(ready) = self.take_held(target_us) {
                self.fed_through_order = None;
                self.key_order = None;
                self.input_timestamps.clear();
                return Ok(Some(ready));
            }
        }
        Ok(None)
    }

    async fn feed_sample(
        &mut self,
        sample: &EncodedSample,
        force_key: bool,
        prune_before_us: i64,
        is_cancelled: &dyn Fn() -> bool,
    ) -> Result<(), MediaError> {
        // Prune before/during backpressure so a long key→target cascade (PREV)
        // does not pin dozens of VideoFrames and wedge decodeQueueSize.
        self.await_decode_capacity(Some(prune_before_us), None, Some(is_cancelled))
            .await?;
        if is_cancelled() {
            return Err(MediaError::Cancelled);
        }
        self.decode_encoded_sample(sample, force_key)
    }

    /// Like [`Self::feed_sample`] but drains remapped frames to `on_frame` during
    /// backpressure waits so `held` stays empty (GOP densify / scrub fill).
    async fn feed_sample_gop(
        &mut self,
        sample: &EncodedSample,
        force_key: bool,
        pts_to_id: &mut HashMap<i64, FrameId>,
        on_frame: &mut dyn FnMut(FrameId, OwnedVideoFrame),
        is_cancelled: &dyn Fn() -> bool,
    ) -> Result<(), MediaError> {
        self.await_decode_capacity(None, Some((pts_to_id, on_frame)), Some(is_cancelled))
            .await?;
        if is_cancelled() {
            return Err(MediaError::Cancelled);
        }
        self.decode_encoded_sample(sample, force_key)
    }

    fn decode_encoded_sample(
        &mut self,
        sample: &EncodedSample,
        force_key: bool,
    ) -> Result<(), MediaError> {
        let chunk_type = if force_key || sample.is_keyframe {
            EncodedVideoChunkType::Key
        } else {
            EncodedVideoChunkType::Delta
        };
        let timestamp_us = ticks_to_micros(sample.composition_timestamp_ticks, sample.timescale);
        self.push_input_timestamp(timestamp_us);
        let timestamp_i32 = timestamp_us.clamp(i32::MIN as i64, i32::MAX as i64) as i32;

        // mediabunny: Chromium first-packet AVC AUD / reserved-NAL sanitize.
        let sanitized =
            if self.packets_fed == 0 && is_chromium() && self.config.codec.starts_with("avc1") {
                avc_nal_length_size(&self.config.description)
                    .and_then(|len_size| sanitize_avc_packet_for_chromium(&sample.data, len_size))
            } else {
                None
            };
        if let Some(ref cleaned) = sanitized {
            #[cfg(target_arch = "wasm32")]
            web_sys::console::log_1(
                &format!(
                    "webmedia: chromium AVC sanitize first packet {}→{} B",
                    sample.data.len(),
                    cleaned.len()
                )
                .into(),
            );
        }
        let bytes = sanitized.as_deref().unwrap_or(sample.data.as_slice());

        let init = EncodedVideoChunkInit::new(&Uint8Array::from(bytes), timestamp_i32, chunk_type);
        let duration_us = ticks_to_micros(sample.duration_ticks as i64, sample.timescale);
        if duration_us > 0 {
            init.set_duration(duration_us.clamp(0, u32::MAX as i64) as u32);
        }
        let chunk = EncodedVideoChunk::new(&init).map_err(MediaError::from)?;
        self.decoder.decode(&chunk).map_err(MediaError::from)?;
        self.packets_fed = self.packets_fed.saturating_add(1);
        self.check_error()
    }

    /// `flush()` raced with a timeout — a wedged Chromium decoder never settles.
    async fn flush_with_timeout(&self) -> Result<(), MediaError> {
        self.flush_with_timeout_ms(2_000).await
    }

    async fn flush_with_timeout_ms(&self, ms: i32) -> Result<(), MediaError> {
        let flush: js_sys::Promise = self.decoder.flush().unchecked_into();
        let result = JsFuture::from(promise_race(flush, sleep_promise_ms(ms))).await;
        match result {
            Ok(v) if v.as_string().as_deref() == Some("timeout") => Err(MediaError::WebCodecs(
                "VideoDecoder.flush timed out (Chromium wedged?)".into(),
            )),
            Ok(_) => Ok(()),
            Err(e) => Err(MediaError::from(e)),
        }
    }

    /// Backpressure before `decode()` — mediabunny `media-sink.ts`:
    /// `sampleQueue.length + decodeQueueSize > computeMaxQueueSize(sampleQueue.length)`
    /// then `await queueDequeue` (resolved on each output **or** consumer take).
    ///
    /// `drop_before_us`: close held frames strictly before the seek target so
    /// `held` does not stay non-empty forever during a long key→N cascade (that
    /// kept max=8 and stalled the browser queue — rapid PREV timeout). While
    /// pruning with an empty held buffer, keep the encoded queue shallow so we
    /// pace on close rather than sit at `decodeQ≈10` with `held=0`.
    ///
    /// `gop`: when densifying a full GOP, drain remapped frames to the consumer
    /// each spin so `held` stays empty (consumer take → max queue 40, not 8).
    ///
    /// Do **not** treat already-buffered `frames` alone as capacity progress
    /// (that spun to timeout while `decodeQueueSize` never dropped).
    async fn await_decode_capacity(
        &mut self,
        drop_before_us: Option<i64>,
        mut gop: Option<(
            &mut HashMap<i64, FrameId>,
            &mut dyn FnMut(FrameId, OwnedVideoFrame),
        )>,
        is_cancelled: Option<&dyn Fn() -> bool>,
    ) -> Result<(), MediaError> {
        let mut spins = 0u32;
        loop {
            if is_cancelled.is_some_and(|f| f()) {
                return Err(MediaError::Cancelled);
            }
            self.check_error()?;
            // Remap + prune before measuring queue pressure.
            if let Some(us) = drop_before_us {
                self.collect_and_prune_held(us)?;
            } else {
                self.collect_outputs_into_held()?;
            }
            if let Some((pts_to_id, on_frame)) = gop.as_mut() {
                self.flush_held_into_gop(*on_frame, pts_to_id);
            }
            let decoded = self.held.len() as u32;
            let mut max = compute_max_queue_size(decoded);
            // Seek prune with nothing kept yet: stay shallow (we close every
            // early output). Empty-queue depth 40 is for play / GOP fill where
            // outputs are retained.
            if drop_before_us.is_some() && decoded == 0 {
                max = max.min(SEEK_PRUNE_MAX_QUEUE);
            }
            // mediabunny: sampleQueue.length + decodeQueueSize > maxQueueSize
            if decoded + self.decode_queue_size() <= max {
                break;
            }
            spins += 1;
            if spins > 600 {
                return Err(MediaError::WebCodecs(format!(
                    "decodeQueueSize backpressure timed out (q={}, held={})",
                    self.decode_queue_size(),
                    self.held.len()
                )));
            }
            self.wait_for_capacity_progress().await;
        }
        self.check_error()
    }

    /// After a feed: give WebCodecs a macrotask to emit; never park forever.
    async fn await_outputs_if_pending(&self) -> Result<(), MediaError> {
        self.check_error()?;
        if !self.frames.borrow().is_empty() {
            return Ok(());
        }
        if self.input_timestamps.is_empty() {
            return Ok(());
        }
        if self.decode_queue_size() == 0 {
            return Ok(());
        }
        self.poll_output_or_macrotask().await;
        self.check_error()
    }

    /// mediabunny: await progress until an output arrives (or a short timer).
    async fn poll_output_or_macrotask(&self) {
        let frames = Rc::clone(&self.frames);
        let output_waiters = Rc::clone(&self.output_waiters);
        let output = js_sys::Promise::new(&mut |resolve, _reject| {
            if !frames.borrow().is_empty() {
                let _ = resolve.call0(&JsValue::NULL);
                return;
            }
            output_waiters.borrow_mut().push(resolve);
        });
        // Macrotask — HW decode often will not finish in a microtask-only spin
        // (that path forced `flush` on Wing It frame 0 and broke Play).
        let tick = sleep_promise_ms(0);
        let _ = JsFuture::from(promise_race(output, tick)).await;
    }

    /// Wait for `decodeQueueSize` to drop: `ondequeue`, new `output`, or macrotask.
    /// Do **not** resolve early when `frames` is already non-empty — that is not
    /// queue progress and caused false backpressure timeouts (mediabunny waits on
    /// real `onQueueDequeue` from a new sample).
    async fn wait_for_capacity_progress(&self) {
        let output_waiters = Rc::clone(&self.output_waiters);
        let dequeue_waiters = Rc::clone(&self.dequeue_waiters);

        let output = js_sys::Promise::new(&mut |resolve, _reject| {
            output_waiters.borrow_mut().push(resolve);
        });
        let dequeue = js_sys::Promise::new(&mut |resolve, _reject| {
            dequeue_waiters.borrow_mut().push(resolve);
        });
        let tick = sleep_promise_ms(0);
        let _ = JsFuture::from(js_sys::Promise::race(&js_sys::Array::of3(
            &output, &dequeue, &tick,
        )))
        .await;
    }

    fn check_error(&self) -> Result<(), MediaError> {
        if let Some(msg) = self.error.borrow_mut().take() {
            Err(MediaError::WebCodecs(msg))
        } else {
            Ok(())
        }
    }
}

impl Drop for WebCodecsDecoder {
    fn drop(&mut self) {
        self.detach_decoder();
        // Leak Closures — same reason as `recreate` (late JS invokes after close).
        let output = std::mem::replace(&mut self._on_output, Closure::new(|_| {}));
        let error = std::mem::replace(&mut self._on_error, Closure::new(|_| {}));
        let dequeue = std::mem::replace(&mut self._on_dequeue, Closure::new(|_| {}));
        std::mem::forget(output);
        std::mem::forget(error);
        std::mem::forget(dequeue);
    }
}

fn wake_waiters_deferred(waiting: Vec<js_sys::Function>) {
    if waiting.is_empty() {
        return;
    }
    // Leave the WebCodecs Closure stack before waking Rust waiters (reentrancy).
    // `spawn_local` is reliable; raw `queueMicrotask(promiseResolve)` can fail to
    // run and leave `open` / decode stuck with `busy=true` and no error.
    for resolve in waiting {
        wasm_bindgen_futures::spawn_local(async move {
            let _ = resolve.call0(&JsValue::NULL);
        });
    }
}

fn ticks_to_micros(ticks: i64, timescale: u64) -> i64 {
    let timescale = timescale.max(1) as i128;
    ((ticks as i128) * 1_000_000 / timescale) as i64
}

/// Macrotask turn via `setTimeout` + bare Promise `resolve` (mediabunny-compatible).
/// Required when waiting for WebCodecs to drain — microtasks alone never let it run.
fn sleep_promise_ms(ms: i32) -> js_sys::Promise {
    js_sys::Promise::new(&mut |resolve, _reject| {
        let window = match web_sys::window() {
            Some(w) => w,
            None => {
                let _ = resolve.call1(&JsValue::NULL, &JsValue::from_str("timeout"));
                return;
            }
        };
        if window
            .set_timeout_with_callback_and_timeout_and_arguments_0(&resolve, ms)
            .is_err()
        {
            let _ = resolve.call1(&JsValue::NULL, &JsValue::from_str("timeout"));
        }
    })
}

fn promise_race(a: js_sys::Promise, b: js_sys::Promise) -> js_sys::Promise {
    js_sys::Promise::race(&js_sys::Array::of2(&a, &b))
}

fn is_chromium() -> bool {
    let Some(window) = web_sys::window() else {
        return false;
    };
    let Ok(nav) = js_sys::Reflect::get(&window, &"navigator".into()) else {
        return false;
    };
    let Ok(ua) = js_sys::Reflect::get(&nav, &"userAgent".into()) else {
        return false;
    };
    let Some(nav) = ua.as_string() else {
        return false;
    };
    // mediabunny `isChromium`: Chrome / Edge / Chromium UA.
    nav.contains("Chrome") || nav.contains("Chromium") || nav.contains("Edg/")
}
