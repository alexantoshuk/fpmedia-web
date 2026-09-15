//! Audio decode + `AudioContext` scheduling (mediabunny media-player pattern).
//!
//! `AudioDecoder` → `AudioData` → `f32-planar` → `AudioBuffer` →
//! `AudioBufferSourceNode` at `ctx_origin + timestamp - media_at_start`.
//! Media time = `context.currentTime - ctx_origin + media_at_start` while playing.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::rc::Rc;

use js_sys::{Float32Array, Promise, Uint8Array};
use wasm_bindgen::closure::Closure;
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;
use web_sys::{
    AudioBuffer, AudioBufferSourceNode, AudioContext, AudioContextOptions, AudioContextState,
    AudioData, AudioDataCopyToOptions, AudioDecoder, AudioDecoderConfig, AudioDecoderInit,
    AudioSampleFormat, AudioScheduledSourceNode, EncodedAudioChunk, EncodedAudioChunkInit,
    EncodedAudioChunkType, GainNode,
};

use crate::decode::owned_audio_data::OwnedAudioData;
use crate::demux::Mp4AudioTrack;
use crate::error::MediaError;
use crate::types::AudioMetadata;

/// How far ahead of media time to keep scheduling before yielding (seconds).
const AHEAD_SECS: f64 = 1.0;

struct PendingSample {
    timestamp_secs: f64,
    data: OwnedAudioData,
}

/// Optional audio sink attached to [`super::WebPlayer`].
pub struct AudioOutput {
    track: Rc<Mp4AudioTrack>,
    context: AudioContext,
    gain: GainNode,
    volume: Cell<f64>,
    muted: Cell<bool>,
    /// Bumped on stop/seek so in-flight pumps exit.
    epoch: Cell<u64>,
    /// `AudioContext.currentTime` when play started.
    ctx_origin: Cell<Option<f64>>,
    media_at_start: Cell<f64>,
    playing: Cell<bool>,
    queued: RefCell<Vec<AudioBufferSourceNode>>,
    pending: Rc<RefCell<VecDeque<PendingSample>>>,
    error: Rc<RefCell<Option<String>>>,
    decoder: RefCell<Option<AudioDecoder>>,
    _on_output: Closure<dyn FnMut(JsValue)>,
    _on_error: Closure<dyn FnMut(JsValue)>,
}

impl AudioOutput {
    pub async fn create(track: Mp4AudioTrack) -> Result<Self, MediaError> {
        let meta = track.metadata().clone();
        let codec = meta
            .codec
            .codec_string
            .clone()
            .ok_or(MediaError::UnsupportedCodec)?;

        let opts = AudioContextOptions::new();
        opts.set_sample_rate(meta.sample_rate as f32);
        let context = AudioContext::new_with_context_options(&opts)?;
        let gain = context.create_gain()?;
        gain.connect_with_audio_node(&context.destination())?;
        gain.gain().set_value(0.7_f32.powi(2));

        let pending: Rc<RefCell<VecDeque<PendingSample>>> = Rc::new(RefCell::new(VecDeque::new()));
        let error: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));

        let pending_out = Rc::clone(&pending);
        let on_output = Closure::wrap(Box::new(move |value: JsValue| {
            let Ok(data) = value.dyn_into::<AudioData>() else {
                return;
            };
            let owned = OwnedAudioData::new(data);
            let timestamp_secs = owned.timestamp() / 1_000_000.0;
            pending_out.borrow_mut().push_back(PendingSample {
                timestamp_secs,
                data: owned,
            });
        }) as Box<dyn FnMut(JsValue)>);

        let error_out = Rc::clone(&error);
        let on_error = Closure::wrap(Box::new(move |value: JsValue| {
            let msg = value
                .as_string()
                .or_else(|| {
                    js_sys::Reflect::get(&value, &"message".into())
                        .ok()
                        .and_then(|v| v.as_string())
                })
                .unwrap_or_else(|| format!("{value:?}"));
            *error_out.borrow_mut() = Some(msg);
        }) as Box<dyn FnMut(JsValue)>);

        let init = AudioDecoderInit::new(
            on_error.as_ref().unchecked_ref(),
            on_output.as_ref().unchecked_ref(),
        );
        let decoder = AudioDecoder::new(&init)?;
        configure_decoder(&decoder, &meta, &codec).await?;

        Ok(Self {
            track: Rc::new(track),
            context,
            gain,
            volume: Cell::new(0.7),
            muted: Cell::new(false),
            epoch: Cell::new(0),
            ctx_origin: Cell::new(None),
            media_at_start: Cell::new(0.0),
            playing: Cell::new(false),
            queued: RefCell::new(Vec::new()),
            pending,
            error,
            decoder: RefCell::new(Some(decoder)),
            _on_output: on_output,
            _on_error: on_error,
        })
    }

    pub fn metadata(&self) -> &AudioMetadata {
        self.track.metadata()
    }

    pub fn set_volume(&self, volume: f64) {
        let v = volume.clamp(0.0, 1.0);
        self.volume.set(v);
        self.apply_gain();
    }

    pub fn volume(&self) -> f64 {
        self.volume.get()
    }

    pub fn set_muted(&self, muted: bool) {
        self.muted.set(muted);
        self.apply_gain();
    }

    pub fn muted(&self) -> bool {
        self.muted.get()
    }

    fn apply_gain(&self) {
        let actual = if self.muted.get() {
            0.0
        } else {
            self.volume.get()
        };
        // Quadratic curve — mediabunny media-player.
        self.gain.gain().set_value((actual as f32).powi(2));
    }

    /// Current media time from the audio context clock while playing.
    pub fn media_time(&self) -> f64 {
        if self.playing.get()
            && let Some(origin) = self.ctx_origin.get()
        {
            return self.context.current_time() - origin + self.media_at_start.get();
        }
        self.media_at_start.get()
    }

    pub fn is_playing(&self) -> bool {
        self.playing.get()
    }

    /// Resume a suspended `AudioContext` (must run close to a user gesture).
    pub async fn resume_context(&self) -> Result<(), MediaError> {
        if self.context.state() == AudioContextState::Suspended {
            JsFuture::from(self.context.resume()?)
                .await
                .map_err(MediaError::from)?;
        }
        let state = self.context.state();
        if state != AudioContextState::Running {
            return Err(MediaError::WebCodecs(format!(
                "AudioContext still {state:?} after resume (autoplay policy?)"
            )));
        }
        Ok(())
    }

    /// Stop scheduled nodes and cancel the decode pump. Returns snapped media time.
    ///
    /// Reentrant-safe: decode/`onended` may re-enter while we tear down.
    pub fn stop(&self) -> f64 {
        let t = self.media_time();
        // Cancel in-flight pump first (checked after every await).
        self.epoch.set(self.epoch.get().wrapping_add(1));
        self.playing.set(false);
        self.ctx_origin.set(None);
        self.media_at_start.set(t);
        self.stop_queued_nodes();
        if let Ok(mut q) = self.pending.try_borrow_mut() {
            q.clear();
        }
        if let Ok(dec) = self.decoder.try_borrow()
            && let Some(dec) = dec.as_ref()
        {
            let _ = dec.reset();
        }
        t
    }

    fn stop_queued_nodes(&self) {
        let Ok(mut queued) = self.queued.try_borrow_mut() else {
            return;
        };
        for node in queued.drain(..) {
            // Ignore errors — node may already have ended.
            let scheduled = node.unchecked_ref::<AudioScheduledSourceNode>();
            let _ = scheduled.stop();
        }
    }

    /// Begin playback from `media_time_secs`. Resumes the context and arms origins;
    /// the decode/schedule pump is started by [`Self::start_spawned`].
    pub async fn begin(&self, media_time_secs: f64) -> Result<u64, MediaError> {
        self.stop();
        self.resume_context().await?;

        let epoch = self.epoch.get();
        self.media_at_start.set(media_time_secs.max(0.0));
        self.ctx_origin.set(Some(self.context.current_time()));
        self.playing.set(true);
        *self.error.borrow_mut() = None;

        let taken = self.decoder.borrow_mut().take();
        if let Some(dec) = taken {
            let meta = self.track.metadata();
            let codec = meta
                .codec
                .codec_string
                .as_deref()
                .ok_or(MediaError::UnsupportedCodec)?;
            configure_decoder(&dec, meta, codec).await?;
            *self.decoder.borrow_mut() = Some(dec);
        }
        web_sys::console::log_1(
            &format!(
                "webmedia: audio begin t={:.3}s rate={} ch={} samples={}",
                media_time_secs,
                self.track.metadata().sample_rate,
                self.track.metadata().number_of_channels,
                self.track.sample_count()
            )
            .into(),
        );
        Ok(epoch)
    }

    /// Decode + schedule loop from `media_time_secs` (cancel via [`Self::stop`]).
    pub async fn run_pump(&self, epoch: u64, media_time_secs: f64) -> Result<(), MediaError> {
        let start_sample = self.track.sample_at_or_after(media_time_secs).unwrap_or(0);
        self.pump(epoch, start_sample).await
    }

    /// Convenience when the caller already holds `Rc<Self>`: begin + spawn pump.
    pub async fn start_spawned(self: &Rc<Self>, media_time_secs: f64) -> Result<(), MediaError> {
        let epoch = self.begin(media_time_secs).await?;
        let this = Rc::clone(self);
        wasm_bindgen_futures::spawn_local(async move {
            if let Err(e) = this.run_pump(epoch, media_time_secs).await
                && !e.is_cancelled()
            {
                web_sys::console::warn_1(&format!("webmedia: audio pump error: {e}").into());
            }
        });
        Ok(())
    }

    async fn pump(&self, epoch: u64, mut sample_idx: u64) -> Result<(), MediaError> {
        let count = self.track.sample_count();
        while sample_idx < count {
            if self.epoch.get() != epoch {
                return Ok(());
            }
            if let Some(err) = self.error.borrow().clone() {
                return Err(MediaError::WebCodecs(err));
            }

            // Drain decoded outputs. Pop under a short borrow — do **not** use
            // `while let … borrow_mut()` (RefMut lives for the whole body and
            // panics when scrub/`stop` clears `pending`).
            loop {
                let next = self.pending.borrow_mut().pop_front();
                let Some(pending) = next else {
                    break;
                };
                if self.epoch.get() != epoch {
                    return Ok(());
                }
                self.schedule_sample(pending.timestamp_secs, &pending.data)?;
                let ts = pending.timestamp_secs;
                // Ahead throttle.
                while ts - self.media_time() >= AHEAD_SECS {
                    if self.epoch.get() != epoch {
                        return Ok(());
                    }
                    yield_timeout_ms(50).await;
                }
            }

            let sample = self.track.encoded_sample(sample_idx).await?;
            if self.epoch.get() != epoch {
                return Ok(());
            }
            self.feed_chunk(&sample)?;
            sample_idx += 1;

            // Wait briefly for decoder output if queue empty.
            if self.pending.borrow().is_empty() {
                yield_timeout_ms(0).await;
            }
        }

        // Flush remaining outputs.
        let taken = self.decoder.borrow_mut().take();
        if let Some(dec) = taken {
            let _ = JsFuture::from(dec.flush()).await;
            *self.decoder.borrow_mut() = Some(dec);
        }
        loop {
            let next = self.pending.borrow_mut().pop_front();
            let Some(pending) = next else {
                break;
            };
            if self.epoch.get() != epoch {
                return Ok(());
            }
            self.schedule_sample(pending.timestamp_secs, &pending.data)?;
        }
        Ok(())
    }

    fn feed_chunk(&self, sample: &crate::types::EncodedAudioSample) -> Result<(), MediaError> {
        let ts_us = (sample.composition_timestamp().seconds * 1_000_000.0).round() as i32;
        let ty = if sample.is_keyframe {
            EncodedAudioChunkType::Key
        } else {
            EncodedAudioChunkType::Delta
        };
        let data = Uint8Array::from(sample.data.as_slice());
        let init = EncodedAudioChunkInit::new(data.as_ref(), ts_us, ty);
        let dur_us =
            (sample.duration_ticks as f64 * 1_000_000.0 / sample.timescale.max(1) as f64).round();
        if dur_us > 0.0 && dur_us < u32::MAX as f64 {
            init.set_duration(dur_us as u32);
        }
        let chunk = EncodedAudioChunk::new(&init)?;
        // Drop the RefCell guard before `decode` — output callback may re-enter.
        {
            let decoder = self.decoder.borrow();
            let dec = decoder
                .as_ref()
                .ok_or_else(|| MediaError::WebCodecs("audio decoder missing".into()))?;
            dec.decode(&chunk)?;
        }
        Ok(())
    }

    fn schedule_sample(
        &self,
        timestamp_secs: f64,
        data: &OwnedAudioData,
    ) -> Result<(), MediaError> {
        let Some(ctx_origin) = self.ctx_origin.get() else {
            return Ok(());
        };
        let buffer = audio_data_to_buffer(&self.context, data)?;
        let duration = buffer.duration();
        let node = self.context.create_buffer_source()?;
        node.set_buffer(Some(&buffer));
        node.connect_with_audio_node(&self.gain)?;

        let sample_rate = self.context.sample_rate() as f64;
        let mut start = ctx_origin + timestamp_secs - self.media_at_start.get();
        // Round to sample boundaries — mediabunny media-player.
        start = (sample_rate * start).round() / sample_rate;

        let now = self.context.current_time();
        if start >= now {
            node.start_with_when(start)?;
        } else {
            let offset = now - start;
            // Past the whole buffer — skip (would be silent / throw).
            if offset >= duration {
                return Ok(());
            }
            node.start_with_when_and_grain_offset(now, offset)?;
        }

        self.queued.borrow_mut().push(node);
        Ok(())
    }
}

impl Drop for AudioOutput {
    fn drop(&mut self) {
        self.epoch.set(self.epoch.get().wrapping_add(1));
        self.stop_queued_nodes();
        let taken = self.decoder.borrow_mut().take();
        if let Some(dec) = taken {
            let _ = dec.close();
        }
        let _ = self.context.close();
    }
}

async fn configure_decoder(
    decoder: &AudioDecoder,
    meta: &AudioMetadata,
    codec: &str,
) -> Result<(), MediaError> {
    let config = AudioDecoderConfig::new(codec, meta.number_of_channels, meta.sample_rate);
    if !meta.codec.description.is_empty() {
        config.set_description(&Uint8Array::from(meta.codec.description.as_slice()));
    }
    let support = JsFuture::from(AudioDecoder::is_config_supported(&config)).await?;
    let supported = js_sys::Reflect::get(&support, &"supported".into())
        .ok()
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    if !supported {
        return Err(MediaError::CodecUnsupported(codec.to_string()));
    }
    decoder.configure(&config)?;
    Ok(())
}

fn audio_data_to_buffer(
    context: &AudioContext,
    data: &OwnedAudioData,
) -> Result<AudioBuffer, MediaError> {
    let channels = data.number_of_channels();
    let frames = data.number_of_frames();
    // Web Audio requires buffer rate == context rate.
    let rate = context.sample_rate();
    let buffer = context.create_buffer(channels, frames, rate)?;

    for ch in 0..channels {
        let opts = AudioDataCopyToOptions::new(ch);
        opts.set_format(AudioSampleFormat::F32Planar);
        let nbytes = data.allocation_size(&opts)?;
        let mut bytes = vec![0u8; nbytes as usize];
        data.copy_to_with_u8_slice(&mut bytes, &opts)?;
        let floats: Vec<f32> = bytes
            .as_chunks::<4>()
            .0
            .iter()
            .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect();
        let f32_arr = Float32Array::new_with_length(floats.len() as u32);
        f32_arr.copy_from(&floats);
        buffer.copy_to_channel_with_f32_array(f32_arr.as_ref(), ch as i32)?;
    }
    Ok(buffer)
}

async fn yield_timeout_ms(ms: i32) {
    let promise = Promise::new(&mut |resolve, _reject| {
        let win = match web_sys::window() {
            Some(w) => w,
            None => {
                let _ = resolve.call0(&JsValue::NULL);
                return;
            }
        };
        let cb = Closure::once_into_js(move || {
            let _ = resolve.call0(&JsValue::NULL);
        });
        let _ = win.set_timeout_with_callback_and_timeout_and_arguments_0(cb.unchecked_ref(), ms);
    });
    let _ = JsFuture::from(promise).await;
}
