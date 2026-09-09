//! WebCodecs `VideoDecoder` wrapper.
//!
//! Playback follows mediabunny's model: feed packets in **decode order**, park
//! outputs in a presentation buffer with remapped PTS, and pull the target
//! frame when ready. Prefer feeding further ahead in the GOP over `flush()`.

use std::cell::RefCell;
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

    fn to_web_config(&self) -> VideoDecoderConfig {
        let config = VideoDecoderConfig::new(&self.codec);
        config.set_coded_width(self.width);
        config.set_coded_height(self.height);
        config.set_description(&Uint8Array::from(self.description.as_slice()));
        // Ask the browser to emit frames ASAP (important with B-frame reorder).
        config.set_optimize_for_latency(true);
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
    frames: Rc<RefCell<Vec<VideoFrame>>>,
    error: Rc<RefCell<Option<String>>>,
    /// Resolvers waiting for the next `output` callback (mediabunny-style).
    output_waiters: Rc<RefCell<Vec<js_sys::Function>>>,
    fed_through_order: Option<usize>,
    key_order: Option<usize>,
    /// Sorted PTS (µs) of fed chunks — shifted onto outputs in presentation order.
    input_timestamps: Vec<i64>,
    held: Vec<HeldFrame>,
    _on_output: Closure<dyn FnMut(JsValue)>,
    _on_error: Closure<dyn FnMut(JsValue)>,
}

impl WebCodecsDecoder {
    pub async fn create(meta: &VideoMetadata) -> Result<Self, MediaError> {
        let config = DecoderConfig::from_meta(meta)?;
        let web_config = config.to_web_config();

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

        Self::from_config(config)
    }

    fn from_config(config: DecoderConfig) -> Result<Self, MediaError> {
        let web_config = config.to_web_config();

        let frames: Rc<RefCell<Vec<VideoFrame>>> = Rc::new(RefCell::new(Vec::new()));
        let error: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));
        let output_waiters: Rc<RefCell<Vec<js_sys::Function>>> =
            Rc::new(RefCell::new(Vec::new()));

        let frames_cb = Rc::clone(&frames);
        let waiters_cb = Rc::clone(&output_waiters);
        let on_output = Closure::wrap(Box::new(move |value: JsValue| {
            if let Ok(frame) = value.dyn_into::<VideoFrame>() {
                frames_cb.borrow_mut().push(frame);
            }
            let waiting = std::mem::take(&mut *waiters_cb.borrow_mut());
            for resolve in waiting {
                let _ = resolve.call0(&JsValue::NULL);
            }
        }) as Box<dyn FnMut(JsValue)>);

        let error_cb = Rc::clone(&error);
        let on_error = Closure::wrap(Box::new(move |value: JsValue| {
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

        let init = VideoDecoderInit::new(
            on_error.as_ref().unchecked_ref(),
            on_output.as_ref().unchecked_ref(),
        );
        let decoder = VideoDecoder::new(&init).map_err(MediaError::from)?;
        decoder.configure(&web_config).map_err(MediaError::from)?;

        Ok(Self {
            decoder,
            config,
            frames,
            error,
            output_waiters,
            fed_through_order: None,
            key_order: None,
            input_timestamps: Vec::new(),
            held: Vec::new(),
            _on_output: on_output,
            _on_error: on_error,
        })
    }

    fn recreate(&mut self) -> Result<(), MediaError> {
        let _ = self.decoder.close();
        for frame in self.frames.borrow_mut().drain(..) {
            frame.close();
        }
        self.clear_held();
        self.input_timestamps.clear();
        self.output_waiters.borrow_mut().clear();
        *self.error.borrow_mut() = None;
        let config = DecoderConfig {
            codec: self.config.codec.clone(),
            width: self.config.width,
            height: self.config.height,
            description: self.config.description.clone(),
        };
        *self = Self::from_config(config)?;
        Ok(())
    }

    pub fn invalidate(&mut self) {
        self.fed_through_order = None;
        self.key_order = None;
        self.input_timestamps.clear();
        self.clear_held();
        self.output_waiters.borrow_mut().clear();
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

        if self.key_order != Some(key_do) {
            self.recreate()?;
            self.key_order = Some(key_do);
            let key_sample = track.encoded_sample(key_id)?;
            self.feed_sample(&key_sample, true)?;
            self.fed_through_order = Some(key_do);
            self.await_outputs_if_pending().await;
            self.collect_outputs_into_held()?;
            if let Some(ready) = self.take_held(target_us) {
                self.drop_held_before(target_us);
                return Ok(ready);
            }
        }

        // Pump decode-order samples until the target PTS lands in `held`.
        // Feeding past the target (next B/P) is intentional — some browsers only
        // emit reordered frames once more packets arrive (mediabunny insight).
        for _ in 0..64 {
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
                    self.await_outputs_if_pending().await;
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
                    continue;
                }
                return Err(MediaError::WebCodecs(format!(
                    "exhausted GOP before pts={target_us}µs (held={})",
                    self.held.len()
                )));
            }

            let sample = &next[0];
            self.feed_sample(sample, false)?;
            self.fed_through_order = Some(sample.decode_order);
            // If the decoder is idle with no output, it is waiting for more input
            // (typical B-frame reorder) — do not block; feed the next packet.
            self.await_outputs_if_pending().await;
        }

        Err(MediaError::WebCodecs(format!(
            "decode pump gave up waiting for pts={target_us}µs"
        )))
    }

    fn feed_sample(&mut self, sample: &EncodedSample, force_key: bool) -> Result<(), MediaError> {
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

    /// Block only while the browser still has in-flight decode work.
    async fn await_outputs_if_pending(&self) {
        if !self.frames.borrow().is_empty() {
            return;
        }
        if self.input_timestamps.is_empty() {
            return;
        }
        self.wait_for_output_or_tick().await;
    }

    async fn wait_for_output_or_tick(&self) {
        yield_event_loop().await;
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
        let _ = self.decoder.close();
        for frame in self.frames.borrow_mut().drain(..) {
            frame.close();
        }
        self.clear_held();
        // Wake any waiters so awaiters don't hang on drop.
        let waiting = std::mem::take(&mut *self.output_waiters.borrow_mut());
        for resolve in waiting {
            let _ = resolve.call0(&JsValue::NULL);
        }
    }
}

fn ticks_to_micros(ticks: i64, timescale: u64) -> i64 {
    let timescale = timescale.max(1) as i128;
    ((ticks as i128) * 1_000_000 / timescale) as i64
}

async fn yield_event_loop() {
    let promise = js_sys::Promise::new(&mut |resolve, _reject| {
        let _ = web_sys::window()
            .expect("window")
            .set_timeout_with_callback_and_timeout_and_arguments_0(&resolve, 0);
    });
    let _ = JsFuture::from(promise).await;
}
