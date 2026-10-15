//! Audio decode + `AudioContext` scheduling (mediabunny media-player pattern).
//!
//! `AudioDecoder` → `AudioData` → `f32-planar` → `AudioBuffer` →
//! `AudioBufferSourceNode` at `ctx_origin + (timestamp - media_at_start) / rate`.
//! Media time = `media_at_start + (context.currentTime - ctx_origin) * rate` while playing.
//!
//! Rate has **no** mediabunny equivalent — invented on top of `getPlaybackTime` /
//! `runAudioIterator` (`examples/media-player/media-player.ts`). Review rates
//! (`<` / `=` [`crate::PLAYBACK_RATE_AUDIO_MAX`]) keep pitch via WSOLA
//! (`crate::audio::TimeStretcher`, Vanilagy gist ≈ Chromium `preservesPitch`);
//! node `playbackRate` stays `1.0`. Above audio max → mute + wall clock
//! ([`crate::is_wall_clock_rate`]).

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::rc::Rc;

use js_sys::{Float32Array, Uint8Array};
use wasm_bindgen::closure::Closure;
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;
use web_sys::{
    AudioBuffer, AudioBufferSourceNode, AudioContext, AudioContextOptions, AudioContextState,
    AudioData, AudioDataCopyToOptions, AudioDecoder, AudioDecoderConfig, AudioDecoderInit,
    AudioSampleFormat, AudioScheduledSourceNode, EncodedAudioChunk, EncodedAudioChunkInit,
    EncodedAudioChunkType, GainNode,
};

use crate::audio::Mp4AudioTrack;
use crate::audio::TimeStretcher;
use crate::audio::owned_audio_data::OwnedAudioData;
use crate::error::MediaError;
use crate::normalize_playback_rate;
use crate::types::AudioMetadata;

/// How far ahead of media time to keep scheduling before yielding (seconds).
const AHEAD_SECS: f64 = 1.0;
/// Seconds of audio to schedule synchronously in [`AudioOutput::restart_warm`]
/// before returning (loop seam / underrun resume — avoid silent video).
const WARM_PRIME_SECS: f64 = 0.08;

enum PendingPayload {
    /// AAC (WebCodecs `AudioData`).
    Decoded(OwnedAudioData),
    /// PCM already converted to f32 planar (`mediabunny: PcmAudioDecoderWrapper`).
    Planar(Vec<Vec<f32>>),
}

struct PendingSample {
    timestamp_secs: f64,
    payload: PendingPayload,
}

/// Optional audio sink attached to [`crate::VideoPlayer`].
pub struct AudioOutput {
    track: Rc<Mp4AudioTrack>,
    context: AudioContext,
    gain: GainNode,
    volume: Cell<f64>,
    muted: Cell<bool>,
    /// Media seconds per context second (Frame.io review range).
    rate: Cell<f64>,
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
    /// Pitch-preserving stretch for review rates ≠ 1×.
    stretch: RefCell<Option<TimeStretcher>>,
    /// Next context time for a stretched chunk (contiguous wall schedule).
    stretch_next_ctx: Cell<f64>,
    _on_output: Closure<dyn FnMut(JsValue)>,
    _on_error: Closure<dyn FnMut(JsValue)>,
}

impl AudioOutput {
    pub async fn create(track: Mp4AudioTrack) -> Result<Self, MediaError> {
        let meta = track.metadata();
        let is_pcm = meta.codec.codec == crate::types::AudioCodec::Pcm;
        if !is_pcm {
            let _ = meta
                .codec
                .codec_string
                .as_ref()
                .ok_or(MediaError::UnsupportedCodec)?;
        } else if meta.codec.pcm.is_none() {
            return Err(MediaError::UnsupportedCodec);
        }

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
                payload: PendingPayload::Decoded(owned),
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

        let decoder = if is_pcm {
            None
        } else {
            let codec = meta
                .codec
                .codec_string
                .as_deref()
                .ok_or(MediaError::UnsupportedCodec)?;
            let init = AudioDecoderInit::new(
                on_error.as_ref().unchecked_ref(),
                on_output.as_ref().unchecked_ref(),
            );
            let decoder = AudioDecoder::new(&init)?;
            configure_decoder(&decoder, &meta, codec).await?;
            Some(decoder)
        };

        Ok(Self {
            track: Rc::new(track),
            context,
            gain,
            volume: Cell::new(0.7),
            muted: Cell::new(false),
            rate: Cell::new(1.0),
            epoch: Cell::new(0),
            ctx_origin: Cell::new(None),
            media_at_start: Cell::new(0.0),
            playing: Cell::new(false),
            queued: RefCell::new(Vec::new()),
            pending,
            error,
            decoder: RefCell::new(decoder),
            stretch: RefCell::new(None),
            stretch_next_ctx: Cell::new(0.0),
            _on_output: on_output,
            _on_error: on_error,
        })
    }

    fn is_pcm(&self) -> bool {
        self.track.metadata().codec.codec == crate::types::AudioCodec::Pcm
    }

    pub fn metadata(&self) -> AudioMetadata {
        self.track.metadata()
    }

    /// fMP4 mfra backfill grew the sample table — keep the pump alive past warm EOF.
    ///
    /// `fresh` is a **delta** fragment (`readFragment`); samples are appended.
    pub fn adopt_fragment_samples(&self, fresh: &Mp4AudioTrack) {
        let before = self.track.sample_count();
        self.track.append_fragment_samples(fresh);
        let after = self.track.sample_count();
        if after > before {
            web_sys::console::log_1(
                &format!("fpmedia-web: audio samples {before}→{after} (mfra backfill)").into(),
            );
        }
    }

    /// Block until AAC bytes through `time_secs` are in the playback cache.
    pub async fn warm_bytes_through(&self, time_secs: f64) -> Result<(), MediaError> {
        self.track.warm_bytes_through(time_secs).await
    }

    pub fn set_expect_more_samples(&self, more: bool) {
        self.track.set_expect_more_samples(more);
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

    pub fn playback_rate(&self) -> f64 {
        self.rate.get()
    }

    /// Store rate without rearming the pump (paused / shuttle hand-off).
    pub fn store_playback_rate(&self, rate: f64) {
        self.rate.set(normalize_playback_rate(rate));
    }

    /// Set review playback rate. While playing, snaps origins and restarts the pump.
    ///
    /// Callers must not invoke this above [`crate::PLAYBACK_RATE_AUDIO_MAX`]
    /// while wanting audio — use [`Self::store_playback_rate`] + stop instead
    /// ([`crate::is_wall_clock_rate`]).
    pub fn set_playback_rate(self: &Rc<Self>, rate: f64) {
        let rate = normalize_playback_rate(rate);
        if crate::is_wall_clock_rate(rate) {
            // Above audio ceiling (e.g. 8×): keep cell updated, never schedule.
            if self.playing.get() {
                let _ = self.stop();
            }
            self.rate.set(rate);
            return;
        }
        if (self.rate.get() - rate).abs() < 1e-12 {
            return;
        }
        if !self.playing.get() {
            self.rate.set(rate);
            return;
        }
        let t = self.media_time();
        self.epoch.set(self.epoch.get().wrapping_add(1));
        let epoch = self.epoch.get();
        self.stop_queued_nodes();
        if let Ok(mut q) = self.pending.try_borrow_mut() {
            q.clear();
        }
        self.clear_stretch();
        self.rate.set(rate);
        self.media_at_start.set(t.max(0.0));
        self.ctx_origin.set(Some(self.context.current_time()));
        self.reset_stretch_for_rate();

        let this = Rc::clone(self);
        wasm_bindgen_futures::spawn_local(async move {
            let taken = this.decoder.borrow_mut().take();
            if let Some(dec) = taken {
                let _ = dec.reset();
                let meta = this.track.metadata();
                let codec = match meta.codec.codec_string.as_deref() {
                    Some(c) => c,
                    None => {
                        *this.decoder.borrow_mut() = Some(dec);
                        return;
                    }
                };
                if let Err(e) = configure_decoder(&dec, &meta, codec).await {
                    web_sys::console::warn_1(
                        &format!("fpmedia-web: audio rate-rearm configure: {e}").into(),
                    );
                    *this.decoder.borrow_mut() = Some(dec);
                    return;
                }
                *this.decoder.borrow_mut() = Some(dec);
            }
            if let Err(e) = this.run_pump(epoch, t).await {
                web_sys::console::warn_1(
                    &format!("fpmedia-web: audio rate-rearm pump: {e}").into(),
                );
            }
        });
    }

    fn clear_stretch(&self) {
        *self.stretch.borrow_mut() = None;
        self.stretch_next_ctx.set(0.0);
    }

    /// Arm WSOLA when rate ≠ 1; clear when exact 1×.
    fn reset_stretch_for_rate(&self) {
        let rate = self.rate.get();
        if (rate - 1.0).abs() < 1e-9 {
            self.clear_stretch();
            return;
        }
        let channels = self.track.metadata().number_of_channels as usize;
        let sample_rate = self.context.sample_rate() as f64;
        let factor = 1.0 / rate.max(1e-6);
        *self.stretch.borrow_mut() = Some(TimeStretcher::new(channels, sample_rate, factor));
        self.stretch_next_ctx
            .set(self.ctx_origin.get().unwrap_or(0.0));
    }

    fn wants_stretch(&self) -> bool {
        (self.rate.get() - 1.0).abs() > 1e-9
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
            let elapsed = self.context.current_time() - origin;
            return self.media_at_start.get() + elapsed.max(0.0) * self.rate.get();
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
    /// Hard stop: also [`AudioDecoder::reset`] so the next [`Self::begin`] reconfigures
    /// (pause / seek). For video underrun / loop seam use [`Self::hold_underrun`] +
    /// [`Self::restart_warm`] instead — those keep the decoder configured.
    ///
    /// Reentrant-safe: decode/`onended` may re-enter while we tear down.
    pub fn stop(&self) -> f64 {
        let t = self.cancel_pump_and_silence();
        if let Ok(dec) = self.decoder.try_borrow()
            && let Some(dec) = dec.as_ref()
        {
            let _ = dec.reset();
        }
        t
    }

    /// Soft hold for video underrun / scrub-while-play: silence + cancel pump,
    /// **keep** decoder configured.
    ///
    /// Resume with [`Self::restart_warm`]. Avoids the audible stop/reconfigure gap
    /// that made EOF / loop / scrub-resume seams feel like "audio-only lag" while
    /// video stayed on-canvas.
    /// `mediabunny: examples/media-player` `pause` keeps the audio path warm;
    /// full iterator recreate is for seeks (our warm `restart_warm` after soft hold).
    /// Hard stop ([`Self::stop`] + [`Self::begin`]) stays for open / EOF / abort-prime.
    pub fn hold_underrun(&self) -> f64 {
        self.cancel_pump_and_silence()
    }

    fn cancel_pump_and_silence(&self) -> f64 {
        let t = self.media_time();
        self.epoch.set(self.epoch.get().wrapping_add(1));
        self.playing.set(false);
        self.ctx_origin.set(None);
        self.media_at_start.set(t);
        self.stop_queued_nodes();
        self.clear_stretch();
        if let Ok(mut q) = self.pending.try_borrow_mut() {
            q.clear();
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
    ///
    /// Hard path (pause→play / seek): resets + reconfigures the decoder.
    pub async fn begin(&self, media_time_secs: f64) -> Result<u64, MediaError> {
        self.stop();
        self.resume_context().await?;

        let epoch = self.epoch.get();
        self.media_at_start.set(media_time_secs.max(0.0));
        self.ctx_origin.set(Some(self.context.current_time()));
        self.playing.set(true);
        *self.error.borrow_mut() = None;
        self.reset_stretch_for_rate();

        if !self.is_pcm() {
            let taken = self.decoder.borrow_mut().take();
            if let Some(dec) = taken {
                let meta = self.track.metadata();
                let codec = meta
                    .codec
                    .codec_string
                    .as_deref()
                    .ok_or(MediaError::UnsupportedCodec)?;
                configure_decoder(&dec, &meta, codec).await?;
                *self.decoder.borrow_mut() = Some(dec);
            }
        }
        web_sys::console::log_1(
            &format!(
                "fpmedia-web: audio begin t={:.3}s rate={} stretch={} ch={} samples={} pcm={}",
                media_time_secs,
                self.rate.get(),
                self.wants_stretch(),
                self.track.metadata().number_of_channels,
                self.track.sample_count(),
                self.is_pcm(),
            )
            .into(),
        );
        Ok(epoch)
    }

    /// Warm restart after [`Self::hold_underrun`] or loop seam — no decoder
    /// teardown. Primes the first scheduled buffer(s) before returning so the
    /// canvas seam is not silent while the pump catches up.
    pub async fn restart_warm(self: &Rc<Self>, media_time_secs: f64) -> Result<(), MediaError> {
        let _ = self.cancel_pump_and_silence();
        self.resume_context().await?;

        let epoch = self.epoch.get();
        let t = media_time_secs.max(0.0);
        self.media_at_start.set(t);
        self.ctx_origin.set(Some(self.context.current_time()));
        self.playing.set(true);
        *self.error.borrow_mut() = None;
        self.reset_stretch_for_rate();

        // Keep the decoder instance; soft reset+configure without repeating
        // `isConfigSupported` (already passed on open). Needed so loop seam
        // can seek to t=0 after an EOF flush without a cold `begin`.
        // PCM has no AudioDecoder — skip.
        if !self.is_pcm() {
            let taken = self.decoder.borrow_mut().take();
            if let Some(dec) = taken {
                let meta = self.track.metadata();
                let codec = meta
                    .codec
                    .codec_string
                    .as_deref()
                    .ok_or(MediaError::UnsupportedCodec)?;
                let _ = dec.reset();
                reconfigure_decoder_fast(&dec, &meta, codec)?;
                *self.decoder.borrow_mut() = Some(dec);
            } else {
                return Err(MediaError::WebCodecs("audio decoder missing".into()));
            }
        }

        web_sys::console::log_1(
            &format!(
                "fpmedia-web: audio restart_warm t={:.3}s rate={}",
                t,
                self.rate.get()
            )
            .into(),
        );

        let start_sample = self.track.sample_at_or_after(t).unwrap_or(0);
        let next = self.prime_schedule(epoch, start_sample).await?;
        let this = Rc::clone(self);
        wasm_bindgen_futures::spawn_local(async move {
            if let Err(e) = this.pump(epoch, next).await
                && !e.is_cancelled()
            {
                web_sys::console::warn_1(
                    &format!("fpmedia-web: audio warm pump error: {e}").into(),
                );
            }
        });
        Ok(())
    }

    /// Decode + schedule until ~[`WARM_PRIME_SECS`] of audio is queued (or EOF).
    /// Returns the next sample index for the background pump.
    async fn prime_schedule(&self, epoch: u64, mut sample_idx: u64) -> Result<u64, MediaError> {
        let prime_until = self.media_at_start.get() + WARM_PRIME_SECS;
        loop {
            let count = self.track.sample_count();
            if sample_idx >= count {
                if self.track.expect_more_samples() {
                    crate::util::yield_now().await;
                    if self.epoch.get() != epoch {
                        return Ok(sample_idx);
                    }
                    continue;
                }
                break;
            }
            if self.epoch.get() != epoch {
                return Ok(sample_idx);
            }
            if let Some(err) = self.error.borrow().clone() {
                return Err(MediaError::WebCodecs(err));
            }

            loop {
                let next = self.pending.borrow_mut().pop_front();
                let Some(pending) = next else {
                    break;
                };
                if self.epoch.get() != epoch {
                    return Ok(sample_idx);
                }
                let ts = pending.timestamp_secs;
                self.schedule_pending_sample(ts, pending.payload)?;
                if ts >= prime_until {
                    return Ok(sample_idx);
                }
            }

            let sample = self.track.encoded_sample(sample_idx).await?;
            if self.epoch.get() != epoch {
                return Ok(sample_idx);
            }
            self.feed_chunk(&sample)?;
            sample_idx += 1;

            if self.pending.borrow().is_empty() {
                crate::timer::sleep_ms(0).await;
            }
        }

        // Drain decoder outputs at EOF of prime window.
        loop {
            let next = self.pending.borrow_mut().pop_front();
            let Some(pending) = next else {
                break;
            };
            if self.epoch.get() != epoch {
                break;
            }
            self.schedule_pending_sample(pending.timestamp_secs, pending.payload)?;
        }
        Ok(sample_idx)
    }

    /// Decode + schedule loop from `media_time_secs` (cancel via [`Self::stop`]).
    pub async fn run_pump(&self, epoch: u64, media_time_secs: f64) -> Result<(), MediaError> {
        let start_sample = self.track.sample_at_or_after(media_time_secs).unwrap_or(0);
        self.pump(epoch, start_sample).await
    }

    /// Convenience when the caller already holds `Rc<Self>`: begin, prime the
    /// first buffers, then spawn the background pump.
    ///
    /// Must await [`Self::prime_schedule`] before returning — otherwise video
    /// presents on a running `AudioContext` clock while AAC is still Range-
    /// fetching (URL: seconds of silent play). Same gate as [`Self::restart_warm`].
    /// `mediabunny: examples/media-player/media-player.ts` `play` → `runAudioIterator`.
    pub async fn start_spawned(self: &Rc<Self>, media_time_secs: f64) -> Result<(), MediaError> {
        let epoch = self.begin(media_time_secs).await?;
        let start_sample = self.track.sample_at_or_after(media_time_secs).unwrap_or(0);
        let next = self.prime_schedule(epoch, start_sample).await?;
        let this = Rc::clone(self);
        wasm_bindgen_futures::spawn_local(async move {
            if let Err(e) = this.pump(epoch, next).await
                && !e.is_cancelled()
            {
                web_sys::console::warn_1(&format!("fpmedia-web: audio pump error: {e}").into());
            }
        });
        Ok(())
    }

    async fn pump(&self, epoch: u64, mut sample_idx: u64) -> Result<(), MediaError> {
        loop {
            let count = self.track.sample_count();
            if sample_idx >= count {
                if self.track.expect_more_samples() {
                    // Warm fMP4: AAC table still short — wait for mfra backfill adopt.
                    crate::util::yield_now().await;
                    if self.epoch.get() != epoch {
                        return Ok(());
                    }
                    continue;
                }
                break;
            }
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
                let ts = pending.timestamp_secs;
                self.schedule_pending_sample(ts, pending.payload)?;
                // Ahead throttle: media timeline at 1×; wall schedule when stretching.
                loop {
                    let ahead = if self.wants_stretch() {
                        let now = self.context.current_time();
                        (self.stretch_next_ctx.get() - now).max(0.0)
                    } else {
                        (ts - self.media_time()).max(0.0)
                    };
                    if ahead < AHEAD_SECS {
                        break;
                    }
                    if self.epoch.get() != epoch {
                        return Ok(());
                    }
                    crate::timer::sleep_ms(50).await;
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
                crate::timer::sleep_ms(0).await;
            }
        }

        // Flush remaining AAC decoder outputs (PCM has none).
        if !self.is_pcm() {
            let taken = self.decoder.borrow_mut().take();
            if let Some(dec) = taken {
                let _ = JsFuture::from(dec.flush()).await;
                *self.decoder.borrow_mut() = Some(dec);
            }
        }
        loop {
            let next = self.pending.borrow_mut().pop_front();
            let Some(pending) = next else {
                break;
            };
            if self.epoch.get() != epoch {
                return Ok(());
            }
            self.schedule_pending_sample(pending.timestamp_secs, pending.payload)?;
        }
        if self.epoch.get() != epoch {
            return Ok(());
        }
        self.finalize_stretch()?;
        Ok(())
    }

    fn schedule_pending_sample(
        &self,
        timestamp_secs: f64,
        payload: PendingPayload,
    ) -> Result<(), MediaError> {
        match payload {
            PendingPayload::Decoded(data) => {
                if self.wants_stretch() {
                    let planar = audio_data_to_planar(&data)?;
                    self.feed_stretch_planar(&planar)
                } else {
                    self.schedule_sample(timestamp_secs, &data)
                }
            }
            PendingPayload::Planar(planar) => {
                if self.wants_stretch() {
                    self.feed_stretch_planar(&planar)
                } else {
                    self.schedule_planar_at(timestamp_secs, &planar)
                }
            }
        }
    }

    fn feed_stretch_planar(&self, planar: &[Vec<f32>]) -> Result<(), MediaError> {
        let out = {
            let mut stretch = self.stretch.borrow_mut();
            let Some(stretcher) = stretch.as_mut() else {
                return Ok(());
            };
            stretcher.append(planar)
        };
        if let Some(channels) = out {
            self.schedule_planar_buffer(&channels)?;
        }
        Ok(())
    }

    /// Schedule f32 planar at media timestamp (1× PCM / stretch-free path).
    fn schedule_planar_at(
        &self,
        timestamp_secs: f64,
        channels: &[Vec<f32>],
    ) -> Result<(), MediaError> {
        let Some(ctx_origin) = self.ctx_origin.get() else {
            return Ok(());
        };
        if channels.is_empty() || channels[0].is_empty() {
            return Ok(());
        }
        let frames = channels[0].len();
        let rate = self.context.sample_rate();
        let buffer = self
            .context
            .create_buffer(channels.len() as u32, frames as u32, rate)?;
        for (ch, data) in channels.iter().enumerate() {
            let f32_arr = Float32Array::new_with_length(data.len() as u32);
            f32_arr.copy_from(data);
            buffer.copy_to_channel_with_f32_array(f32_arr.as_ref(), ch as i32)?;
        }
        let start = ctx_origin + (timestamp_secs - self.media_at_start.get());
        self.schedule_buffer_at(buffer, start, 1.0)
    }

    fn finalize_stretch(&self) -> Result<(), MediaError> {
        let out = {
            let mut stretch = self.stretch.borrow_mut();
            match stretch.as_mut() {
                Some(stretcher) => stretcher.finalize(),
                None => return Ok(()),
            }
        };
        // Drop stretcher after finalize so a late feed cannot append.
        *self.stretch.borrow_mut() = None;
        if let Some(channels) = out {
            self.schedule_planar_buffer(&channels)?;
        }
        Ok(())
    }

    fn schedule_planar_buffer(&self, channels: &[Vec<f32>]) -> Result<(), MediaError> {
        if channels.is_empty() || channels[0].is_empty() {
            return Ok(());
        }
        let frames = channels[0].len();
        let rate = self.context.sample_rate();
        let duration = frames as f64 / rate as f64;
        let buffer = self
            .context
            .create_buffer(channels.len() as u32, frames as u32, rate)?;
        for (ch, data) in channels.iter().enumerate() {
            let f32_arr = Float32Array::new_with_length(data.len() as u32);
            f32_arr.copy_from(data);
            buffer.copy_to_channel_with_f32_array(f32_arr.as_ref(), ch as i32)?;
        }
        let start = self.stretch_next_ctx.get();
        let now = self.context.current_time();
        self.schedule_buffer_at(buffer, start, 1.0)?;
        if start >= now {
            self.stretch_next_ctx.set(start + duration);
        } else {
            let offset = now - start;
            if offset >= duration {
                self.stretch_next_ctx.set(now);
            } else {
                self.stretch_next_ctx.set(now + duration - offset);
            }
        }
        Ok(())
    }

    fn schedule_buffer_at(
        &self,
        buffer: AudioBuffer,
        start: f64,
        node_rate: f64,
    ) -> Result<(), MediaError> {
        let duration = buffer.duration();
        let node = self.context.create_buffer_source()?;
        node.set_buffer(Some(&buffer));
        node.connect_with_audio_node(&self.gain)?;
        let sample_rate = self.context.sample_rate() as f64;
        // Round to sample boundaries — mediabunny media-player.
        let start = (sample_rate * start).round() / sample_rate;
        node.playback_rate().set_value(node_rate as f32);
        let now = self.context.current_time();
        if start >= now {
            node.start_with_when(start)?;
        } else {
            let offset = (now - start) * node_rate;
            if offset >= duration {
                return Ok(());
            }
            node.start_with_when_and_grain_offset(now, offset)?;
        }
        self.queued.borrow_mut().push(node);
        Ok(())
    }

    fn feed_chunk(&self, sample: &crate::types::EncodedAudioSample) -> Result<(), MediaError> {
        if self.is_pcm() {
            let meta = self.track.metadata();
            let format = meta.codec.pcm.ok_or(MediaError::UnsupportedCodec)?;
            let planar = format.decode_interleaved_f32(&sample.data, meta.number_of_channels)?;
            self.pending.borrow_mut().push_back(PendingSample {
                timestamp_secs: sample.composition_timestamp().seconds,
                payload: PendingPayload::Planar(planar),
            });
            return Ok(());
        }

        let ts_us = (sample.composition_timestamp().seconds * 1_000_000.0).round() as i32;
        let ty = if sample.is_keyframe {
            EncodedAudioChunkType::Key
        } else {
            EncodedAudioChunkType::Delta
        };
        // EncodedAudioChunk::new copies synchronously — view avoids a pre-copy.
        let data = unsafe { Uint8Array::view(sample.data.as_ref()) };
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
        // Exact 1× — native buffer duration; pitch unchanged.
        let start = ctx_origin + (timestamp_secs - self.media_at_start.get());
        self.schedule_buffer_at(buffer, start, 1.0)
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

/// Reconfigure after [`AudioDecoder::reset`] without a second support probe
/// (open already validated the codec). Used by warm restart / loop seam.
fn reconfigure_decoder_fast(
    decoder: &AudioDecoder,
    meta: &AudioMetadata,
    codec: &str,
) -> Result<(), MediaError> {
    let config = AudioDecoderConfig::new(codec, meta.number_of_channels, meta.sample_rate);
    if !meta.codec.description.is_empty() {
        config.set_description(&Uint8Array::from(meta.codec.description.as_slice()));
    }
    decoder.configure(&config)?;
    Ok(())
}

fn audio_data_to_planar(data: &OwnedAudioData) -> Result<Vec<Vec<f32>>, MediaError> {
    let channels = data.number_of_channels();
    let mut planar = Vec::with_capacity(channels as usize);
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
        planar.push(floats);
    }
    Ok(planar)
}

fn audio_data_to_buffer(
    context: &AudioContext,
    data: &OwnedAudioData,
) -> Result<AudioBuffer, MediaError> {
    let planar = audio_data_to_planar(data)?;
    let channels = planar.len() as u32;
    let frames = planar.first().map(|c| c.len()).unwrap_or(0) as u32;
    let rate = context.sample_rate();
    let buffer = context.create_buffer(channels, frames, rate)?;
    for (ch, floats) in planar.iter().enumerate() {
        let f32_arr = Float32Array::new_with_length(floats.len() as u32);
        f32_arr.copy_from(floats);
        buffer.copy_to_channel_with_f32_array(f32_arr.as_ref(), ch as i32)?;
    }
    Ok(buffer)
}
