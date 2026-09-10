//! WebCodecs `VideoDecoder` wrapper.
//!
//! Playback follows mediabunny's model: feed packets in **decode order**, park
//! outputs in a presentation buffer with remapped PTS, and pull the target
//! frame when ready. Prefer feeding further ahead in the GOP over `flush()`.
//! Apply `decodeQueueSize` backpressure before feeding more packets.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use js_sys::Uint8Array;
use wasm_bindgen::closure::Closure;
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;
use web_sys::{
    EncodedVideoChunk, EncodedVideoChunkInit, EncodedVideoChunkType, VideoDecoder,
    VideoDecoderConfig, VideoDecoderInit, VideoFrame,
};

use crate::demux::Mp4VideoTrack;
use crate::error::MediaError;
use crate::types::{EncodedSample, FrameId, VideoMetadata};

/// Max in-flight encoded chunks when decoded frames are already buffered
/// (mediabunny `computeMaxQueueSize` when `decodedSampleQueueSize > 0`).
const MAX_DECODE_QUEUE_WITH_HELD: u32 = 8;
/// Cap when the held/output buffers are empty — some browsers need a deeper
/// encoded queue before emitting reordered frames.
const MAX_DECODE_QUEUE_EMPTY: u32 = 40;

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
    frame: VideoFrame,
}

/// Decode presentation frames via WebCodecs with a warm decode-order pump.
pub struct WebCodecsDecoder {
    decoder: VideoDecoder,
    config: DecoderConfig,
    optimize_for_latency: bool,
    /// Bumped on teardown so late browser callbacks become no-ops.
    generation: Rc<Cell<u64>>,
    frames: Rc<RefCell<Vec<VideoFrame>>>,
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
    _on_output: Closure<dyn FnMut(JsValue)>,
    _on_error: Closure<dyn FnMut(JsValue)>,
    _on_dequeue: Closure<dyn FnMut(JsValue)>,
}

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

    fn from_config(
        config: DecoderConfig,
        optimize_for_latency: bool,
    ) -> Result<Self, MediaError> {
        let web_config = config.to_web_config(optimize_for_latency);

        let generation = Rc::new(Cell::new(0));
        let frames: Rc<RefCell<Vec<VideoFrame>>> = Rc::new(RefCell::new(Vec::new()));
        let error: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));
        let output_waiters: Rc<RefCell<Vec<js_sys::Function>>> =
            Rc::new(RefCell::new(Vec::new()));
        let dequeue_waiters: Rc<RefCell<Vec<js_sys::Function>>> =
            Rc::new(RefCell::new(Vec::new()));

        let gen_cb = Rc::clone(&generation);
        let my_gen = gen_cb.get();
        let frames_cb = Rc::clone(&frames);
        let waiters_cb = Rc::clone(&output_waiters);
        let on_output = Closure::wrap(Box::new(move |value: JsValue| {
            if gen_cb.get() != my_gen {
                if let Ok(frame) = value.dyn_into::<VideoFrame>() {
                    frame.close();
                }
                return;
            }
            if let Ok(frame) = value.dyn_into::<VideoFrame>() {
                frames_cb.borrow_mut().push(frame);
            }
            let waiting = std::mem::take(&mut *waiters_cb.borrow_mut());
            // Defer resolves — sync wake from inside this Closure re-enters Rust
            // (decode → recreate → drop Closure) and panics with
            // "closure invoked recursively or after being dropped".
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
            frame.close();
        }
        self.clear_held();
        self.input_timestamps.clear();
        let waiting = std::mem::take(&mut *self.output_waiters.borrow_mut());
        wake_waiters_deferred(waiting);
        let waiting = std::mem::take(&mut *self.dequeue_waiters.borrow_mut());
        wake_waiters_deferred(waiting);
        *self.error.borrow_mut() = None;
    }

    fn recreate(&mut self) -> Result<(), MediaError> {
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
        Ok(())
    }

    pub fn invalidate(&mut self) {
        self.fed_through_order = None;
        self.key_order = None;
        self.input_timestamps.clear();
        self.clear_held();
        let waiting = std::mem::take(&mut *self.output_waiters.borrow_mut());
        wake_waiters_deferred(waiting);
        let waiting = std::mem::take(&mut *self.dequeue_waiters.borrow_mut());
        wake_waiters_deferred(waiting);
        for frame in self.frames.borrow_mut().drain(..) {
            frame.close();
        }
    }

    fn clear_held(&mut self) {
        for held in self.held.drain(..) {
            held.frame.close();
        }
    }

    fn push_input_timestamp(&mut self, pts_us: i64) {
        let i = self.input_timestamps.partition_point(|&t| t < pts_us);
        self.input_timestamps.insert(i, pts_us);
    }

    fn collect_outputs_into_held(&mut self) -> Result<(), MediaError> {
        for frame in self.frames.borrow_mut().drain(..) {
            let Some(pts_us) = self.input_timestamps.first().copied() else {
                frame.close();
                return Err(MediaError::WebCodecs(
                    "decoded frame without queued input timestamp".into(),
                ));
            };
            self.input_timestamps.remove(0);
            self.held.push(HeldFrame { pts_us, frame });
        }
        Ok(())
    }

    fn take_held(&mut self, target_us: i64) -> Option<VideoFrame> {
        let idx = self.held.iter().position(|h| h.pts_us == target_us)?;
        Some(self.held.remove(idx).frame)
    }

    fn drop_held_before(&mut self, target_us: i64) {
        let mut i = 0;
        while i < self.held.len() {
            if self.held[i].pts_us < target_us {
                self.held.swap_remove(i).frame.close();
            } else {
                i += 1;
            }
        }
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
    pub async fn decode_frame(
        &mut self,
        track: &Mp4VideoTrack,
        frame: FrameId,
    ) -> Result<VideoFrame, MediaError> {
        let entry = track
            .index()
            .get(frame)
            .ok_or(MediaError::FrameOutOfRange(frame, track.frame_count()))?;
        let target_us = ticks_to_micros(entry.composition_timestamp_ticks, entry.timescale);
        let key_id = track
            .index()
            .nearest_keyframe(frame)
            .ok_or(MediaError::FrameOutOfRange(frame, track.frame_count()))?;
        let key_do = track
            .index()
            .get(key_id)
            .map(|e| e.decode_order)
            .ok_or(MediaError::FrameOutOfRange(key_id, track.frame_count()))?;

        self.collect_outputs_into_held()?;
        if let Some(ready) = self.take_held(target_us) {
            self.drop_held_before(target_us);
            return Ok(ready);
        }

        // Restart when the GOP changes, or when the target was already emitted and
        // discarded (e.g. presentation queue cleared after prefetch) — WebCodecs
        // cannot re-emit a consumed frame without cascading from the key again.
        let target_consumed = self.fed_through_order.is_some_and(|fed| {
            entry.decode_order <= fed && !self.input_timestamps.contains(&target_us)
        });
        if self.key_order != Some(key_do) || target_consumed {
            // Hard reset on new sync — soft flush path hung open on some clips.
            self.recreate()?;
            self.key_order = Some(key_do);
            let key_sample = track.encoded_sample(key_id)?;
            self.feed_sample(&key_sample, true).await?;
            self.fed_through_order = Some(key_do);
            self.await_outputs_if_pending().await?;
            self.collect_outputs_into_held()?;
            if let Some(ready) = self.take_held(target_us) {
                self.drop_held_before(target_us);
                return Ok(ready);
            }
        }

        // Pump decode-order samples until the target PTS lands in `held`.
        // Feeding past the target (next B/P) is intentional — some browsers only
        // emit reordered frames once more packets arrive (mediabunny insight).
        // Cap at track length: Resolve / Telegram exports often use GOPs ≫ 64.
        let max_feeds = (track.frame_count() as usize).saturating_add(32).max(64);
        for _ in 0..max_feeds {
            self.check_error()?;
            self.collect_outputs_into_held()?;
            if let Some(ready) = self.take_held(target_us) {
                self.drop_held_before(target_us);
                return Ok(ready);
            }

            let fed = self.fed_through_order.unwrap_or(key_do);
            let next = track.samples_after_in_gop(key_do, fed, 1)?;
            if next.is_empty() {
                if !self.frames.borrow().is_empty() || !self.input_timestamps.is_empty() {
                    self.await_outputs_if_pending().await?;
                    self.collect_outputs_into_held()?;
                    if let Some(ready) = self.take_held(target_us) {
                        self.drop_held_before(target_us);
                        return Ok(ready);
                    }
                    if !self.input_timestamps.is_empty() {
                        // End of GOP/container: some browsers keep the final frame buffered
                        // until an explicit flush. Only do this once we've run out of input.
                        JsFuture::from(self.decoder.flush())
                            .await
                            .map_err(MediaError::from)?;
                        self.check_error()?;
                        self.collect_outputs_into_held()?;
                        if let Some(ready) = self.take_held(target_us) {
                            self.drop_held_before(target_us);
                            self.fed_through_order = None;
                            self.key_order = None;
                            self.input_timestamps.clear();
                            return Ok(ready);
                        }
                    }
                    return Err(MediaError::WebCodecs(format!(
                        "flush/idle without pts={target_us}µs (held={})",
                        self.held.len()
                    )));
                }
                return Err(MediaError::WebCodecs(format!(
                    "exhausted GOP before pts={target_us}µs (held={})",
                    self.held.len()
                )));
            }

            let sample = &next[0];
            self.feed_sample(sample, false).await?;
            self.fed_through_order = Some(sample.decode_order);
            // If the decoder is idle with no output, it is waiting for more input
            // (typical B-frame reorder) — do not block; feed the next packet.
            self.await_outputs_if_pending().await?;
        }

        Err(MediaError::WebCodecs(format!(
            "decode pump gave up waiting for pts={target_us}µs"
        )))
    }

    async fn feed_sample(
        &mut self,
        sample: &EncodedSample,
        force_key: bool,
    ) -> Result<(), MediaError> {
        self.await_decode_capacity().await?;
        let chunk_type = if force_key || sample.is_keyframe {
            EncodedVideoChunkType::Key
        } else {
            EncodedVideoChunkType::Delta
        };
        let timestamp_us = ticks_to_micros(sample.composition_timestamp_ticks, sample.timescale);
        self.push_input_timestamp(timestamp_us);
        let timestamp_i32 = timestamp_us.clamp(i32::MIN as i64, i32::MAX as i64) as i32;
        let init = EncodedVideoChunkInit::new(
            &Uint8Array::from(sample.data.as_slice()),
            timestamp_i32,
            chunk_type,
        );
        let duration_us = ticks_to_micros(sample.duration_ticks as i64, sample.timescale);
        if duration_us > 0 {
            init.set_duration(duration_us.clamp(0, u32::MAX as i64) as u32);
        }
        let chunk = EncodedVideoChunk::new(&init).map_err(MediaError::from)?;
        self.decoder.decode(&chunk).map_err(MediaError::from)?;
        self.check_error()
    }

    /// Mediabunny-style backpressure on the **browser** encoded queue only.
    /// Do not include `held` — a full held buffer with an idle decoder would
    /// wait forever for `dequeue` that never comes (B-frame reorder deadlock).
    async fn await_decode_capacity(&self) -> Result<(), MediaError> {
        let max = if self.held.is_empty() && self.frames.borrow().is_empty() {
            MAX_DECODE_QUEUE_EMPTY
        } else {
            MAX_DECODE_QUEUE_WITH_HELD
        };
        while self.decode_queue_size() > max {
            self.check_error()?;
            self.wait_for_dequeue_or_tick().await;
        }
        self.check_error()
    }

    /// After a feed: never block the pump when the decoder still needs more
    /// input for B-frame reorder. Waiting forever on `output` while
    /// `decodeQueueSize > 0` hangs open on `with_b_frames`.
    async fn await_outputs_if_pending(&self) -> Result<(), MediaError> {
        self.check_error()?;
        if !self.frames.borrow().is_empty() {
            return Ok(());
        }
        if self.input_timestamps.is_empty() {
            return Ok(());
        }
        if self.decode_queue_size() == 0 {
            // Idle queue + pending timestamps ⇒ reorder needs more input.
            return Ok(());
        }
        // One event-loop turn so an already-queued output can land; then feed on.
        self.wait_for_output_or_tick().await;
        self.check_error()
    }

    async fn wait_for_dequeue_or_tick(&self) {
        let waiters = Rc::clone(&self.dequeue_waiters);
        let dequeue = js_sys::Promise::new(&mut |resolve, _reject| {
            waiters.borrow_mut().push(resolve);
        });
        let _ = JsFuture::from(promise_race(dequeue, yield_microtask())).await;
    }

    async fn wait_for_output_or_tick(&self) {
        let frames = Rc::clone(&self.frames);
        let waiters = Rc::clone(&self.output_waiters);
        let output = js_sys::Promise::new(&mut |resolve, _reject| {
            // Avoid missing an output that arrived between the empty-check and register.
            if !frames.borrow().is_empty() {
                let _ = resolve.call0(&JsValue::NULL);
                return;
            }
            waiters.borrow_mut().push(resolve);
        });
        let _ = JsFuture::from(promise_race(output, yield_microtask())).await;
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

/// One event-loop turn. Prefer `setTimeout(0)` — `Promise.resolve` + some
/// waiter bridges hung `open`/`seek` on Chromium + wasm-bindgen-futures.
fn yield_microtask() -> js_sys::Promise {
    js_sys::Promise::new(&mut |resolve, _reject| {
        let _ = web_sys::window()
            .expect("window")
            .set_timeout_with_callback_and_timeout_and_arguments_0(&resolve, 0);
    })
}

fn promise_race(a: js_sys::Promise, b: js_sys::Promise) -> js_sys::Promise {
    js_sys::Promise::race(&js_sys::Array::of2(&a, &b))
}
