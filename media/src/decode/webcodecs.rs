//! WebCodecs `VideoDecoder` wrapper.

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

use crate::error::MediaError;
use crate::player::DecodeWindow;
use crate::types::{EncodedSample, VideoMetadata};

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
        config
    }
}

/// Decode encoded cascades to [`VideoFrame`]s via the browser WebCodecs API.
pub struct WebCodecsDecoder {
    decoder: VideoDecoder,
    config: DecoderConfig,
    frames: Rc<RefCell<Vec<VideoFrame>>>,
    error: Rc<RefCell<Option<String>>>,
    /// Highest `decode_order` fed since last keyframe restart.
    fed_through_order: Option<usize>,
    /// Keyframe `decode_order` for the current warm GOP.
    key_order: Option<usize>,
    /// Kept alive for the decoder callbacks.
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

        let frames_cb = Rc::clone(&frames);
        let on_output = Closure::wrap(Box::new(move |value: JsValue| {
            if let Ok(frame) = value.dyn_into::<VideoFrame>() {
                frames_cb.borrow_mut().push(frame);
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

        // web-sys `VideoDecoderInit::new` argument order is (error, output).
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
            fed_through_order: None,
            key_order: None,
            _on_output: on_output,
            _on_error: on_error,
        })
    }

    fn recreate(&mut self) -> Result<(), MediaError> {
        let _ = self.decoder.close();
        for frame in self.frames.borrow_mut().drain(..) {
            frame.close();
        }
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

    /// Invalidate warm state (e.g. before backward seek).
    pub fn invalidate(&mut self) {
        self.fed_through_order = None;
        self.key_order = None;
    }

    /// Decode a keyframe cascade and return the frame matching `window.target_frame`.
    ///
    /// Reuses the warm decoder when advancing forward in the same GOP.
    /// Does **not** `flush()` between frames — after `flush()` Chrome requires a
    /// new key chunk, which breaks incremental playback.
    pub async fn decode_window(&mut self, window: &DecodeWindow) -> Result<VideoFrame, MediaError> {
        let target = window
            .samples
            .iter()
            .find(|s| s.frame == window.target_frame)
            .ok_or_else(|| MediaError::WebCodecs("target sample missing from window".into()))?
            .clone();
        let key = window
            .samples
            .first()
            .ok_or_else(|| MediaError::WebCodecs("empty decode window".into()))?
            .clone();

        let can_continue = self.key_order == Some(key.decode_order)
            && self
                .fed_through_order
                .is_some_and(|fed| target.decode_order > fed);

        let to_feed: Vec<EncodedSample> = if can_continue {
            let fed = self.fed_through_order.unwrap_or(0);
            window
                .samples
                .iter()
                .filter(|s| s.decode_order > fed)
                .cloned()
                .collect()
        } else {
            self.recreate()?;
            self.key_order = Some(key.decode_order);
            self.fed_through_order = None;
            window.samples.clone()
        };

        // Drop pending outputs from a previous wait; keep decoder reference state.
        for frame in self.frames.borrow_mut().drain(..) {
            frame.close();
        }

        if to_feed.is_empty() {
            return Err(MediaError::WebCodecs(
                "nothing to feed for target frame".into(),
            ));
        }

        // First chunk after recreate must be a key chunk.
        let force_key_order = if !can_continue {
            Some(key.decode_order)
        } else {
            None
        };

        for sample in &to_feed {
            self.decode_sample(sample, force_key_order == Some(sample.decode_order))?;
        }

        if let Some(last) = to_feed.last() {
            let fed = self.fed_through_order.unwrap_or(0);
            self.fed_through_order = Some(fed.max(last.decode_order));
        }

        let target_us = ticks_to_micros(target.composition_timestamp_ticks, target.timescale);
        match self.wait_for_timestamp(target_us).await {
            Ok(frame) => Ok(frame),
            Err(err) => {
                // Warm state may be poisoned (e.g. after emergency flush).
                self.invalidate();
                Err(err)
            }
        }
    }

    fn decode_sample(&self, sample: &EncodedSample, force_key: bool) -> Result<(), MediaError> {
        let chunk_type = if force_key || sample.is_keyframe {
            EncodedVideoChunkType::Key
        } else {
            EncodedVideoChunkType::Delta
        };
        let timestamp_us = ticks_to_micros(sample.composition_timestamp_ticks, sample.timescale);
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

    /// Poll the output queue until the target presentation timestamp appears.
    async fn wait_for_timestamp(&mut self, target_us: i64) -> Result<VideoFrame, MediaError> {
        for _ in 0..120 {
            self.check_error()?;
            if let Some(frame) = take_matching_frame(&self.frames, target_us) {
                return Ok(frame);
            }
            // Let the browser deliver VideoDecoder output callbacks.
            yield_event_loop().await;
        }

        self.check_error()?;
        // Last resort: drain remaining outputs. After flush Chrome wants a key
        // chunk again — drop warm state so the next call restarts from keyframe.
        JsFuture::from(self.decoder.flush())
            .await
            .map_err(MediaError::from)?;
        self.check_error()?;
        self.invalidate();

        if let Some(frame) = take_matching_frame(&self.frames, target_us) {
            return Ok(frame);
        }
        let mut frames = std::mem::take(&mut *self.frames.borrow_mut());
        pick_closest(&mut frames, target_us)
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
    }
}

fn take_matching_frame(
    frames: &Rc<RefCell<Vec<VideoFrame>>>,
    target_us: i64,
) -> Option<VideoFrame> {
    let mut slot = frames.borrow_mut();
    let idx = slot
        .iter()
        .position(|f| (f.timestamp() - target_us as f64).abs() < 0.5)?;
    let chosen = slot.remove(idx);
    // Close older presentation frames we skipped; keep later ones briefly in case
    // of timestamp jitter (they'll be drained on next clear).
    Some(chosen)
}

fn pick_closest(frames: &mut Vec<VideoFrame>, target_us: i64) -> Result<VideoFrame, MediaError> {
    if frames.is_empty() {
        return Err(MediaError::WebCodecs(
            "decoder produced no frames".into(),
        ));
    }
    let chosen_idx = frames
        .iter()
        .enumerate()
        .min_by(|(_, a), (_, b)| {
            let da = (a.timestamp() - target_us as f64).abs();
            let db = (b.timestamp() - target_us as f64).abs();
            da.partial_cmp(&db).unwrap_or(std::cmp::Ordering::Equal)
        })
        .map(|(i, _)| i)
        .unwrap_or(0);
    let chosen = frames.swap_remove(chosen_idx);
    for frame in frames.drain(..) {
        frame.close();
    }
    Ok(chosen)
}

async fn yield_event_loop() {
    let promise = js_sys::Promise::new(&mut |resolve, _reject| {
        let _ = web_sys::window()
            .expect("window")
            .set_timeout_with_callback_and_timeout_and_arguments_0(&resolve, 0);
    });
    let _ = JsFuture::from(promise).await;
}

fn ticks_to_micros(ticks: i64, timescale: u64) -> i64 {
    let timescale = timescale.max(1) as i128;
    ((ticks as i128) * 1_000_000 / timescale) as i64
}
