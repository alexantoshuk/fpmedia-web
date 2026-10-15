//! Silent media timeline (no AAC track).
//!
//! mediabunny `examples/media-player/media-player.ts` `getPlaybackTime` uses
//! `audioContext.currentTime` even without an audio track. We still open a
//! silent [`AudioContext`] (resume under the Play gesture / autoplay policy)
//! and keep a muted graph alive, but **advance media time from wall
//! `performance.now`**.
//!
//! **Why diverge:** on Chromium, `AudioContext.currentTime` lags wall by
//! ~0.75–0.87× under heavy WebCodecs load when nothing audible is scheduled
//! (ChG-class PCM-ignored masters). Keep-alive `GainNode` + `ConstantSourceNode`
//! improved but did not reach 1×. AAC [`super::AudioOutput`] is unchanged
//! (real buffers keep the audio clock honest).
//! mediabunny: `media-player.ts` `getPlaybackTime` / `gainNode` — no silent-load
//! throttle equivalent.

use std::cell::Cell;

use wasm_bindgen_futures::JsFuture;
use web_sys::{AudioContext, AudioContextOptions, AudioContextState, ConstantSourceNode, GainNode};

use crate::error::MediaError;
use crate::timer::now_ms;

/// Timeline driver when [`super::AudioOutput`] is absent.
pub struct PlaybackClock {
    context: AudioContext,
    /// Silent path to destination (mediabunny `gainNode.connect`).
    _keep_alive_gain: GainNode,
    /// Active source so the context stays in a rendering state.
    _keep_alive_source: ConstantSourceNode,
    /// Wall seconds (`performance.now()/1000`) when [`Self::start`] armed.
    wall_origin: Cell<Option<f64>>,
    media_at_start: Cell<f64>,
    rate: Cell<f64>,
    running: Cell<bool>,
}

impl PlaybackClock {
    pub fn create() -> Result<Self, MediaError> {
        let opts = AudioContextOptions::new();
        let context = AudioContext::new_with_context_options(&opts).map_err(MediaError::from)?;
        let keep_alive_gain = context.create_gain().map_err(MediaError::from)?;
        keep_alive_gain
            .connect_with_audio_node(&context.destination())
            .map_err(MediaError::from)?;
        keep_alive_gain.gain().set_value(0.0);
        let keep_alive_source = context.create_constant_source().map_err(MediaError::from)?;
        keep_alive_source.offset().set_value(0.0);
        keep_alive_source
            .connect_with_audio_node(&keep_alive_gain)
            .map_err(MediaError::from)?;
        keep_alive_source.start().map_err(MediaError::from)?;
        Ok(Self {
            context,
            _keep_alive_gain: keep_alive_gain,
            _keep_alive_source: keep_alive_source,
            wall_origin: Cell::new(None),
            media_at_start: Cell::new(0.0),
            rate: Cell::new(1.0),
            running: Cell::new(false),
        })
    }

    pub fn set_playback_rate(&self, rate: f64) {
        self.rate.set(crate::normalize_playback_rate(rate));
    }

    pub fn media_time(&self) -> f64 {
        if self.running.get()
            && let Some(origin) = self.wall_origin.get()
            && let Some(now) = now_ms()
        {
            let elapsed = (now / 1000.0 - origin).max(0.0);
            return self.media_at_start.get() + elapsed * self.rate.get();
        }
        self.media_at_start.get()
    }

    pub fn is_running(&self) -> bool {
        self.running.get()
    }

    /// Resume while the Play gesture is still warm (autoplay policy).
    pub async fn resume_context(&self) -> Result<(), MediaError> {
        if self.context.state() == AudioContextState::Suspended {
            JsFuture::from(self.context.resume()?)
                .await
                .map_err(MediaError::from)?;
        }
        let state = self.context.state();
        if state != AudioContextState::Running {
            return Err(MediaError::WebCodecs(format!(
                "PlaybackClock AudioContext still {state:?} after resume (autoplay policy?)"
            )));
        }
        Ok(())
    }

    /// Arm the wall clock at `media_time_secs` (after [`Self::resume_context`]).
    pub fn start(&self, media_time_secs: f64) {
        self.media_at_start.set(media_time_secs.max(0.0));
        self.wall_origin.set(now_ms().map(|ms| ms / 1000.0));
        self.running.set(true);
    }

    /// Snap media time and stop advancing.
    pub fn stop(&self) -> f64 {
        let t = self.media_time();
        self.running.set(false);
        self.wall_origin.set(None);
        self.media_at_start.set(t);
        t
    }

    /// Snap to an explicit media time without running.
    pub fn snap(&self, media_time_secs: f64) {
        self.running.set(false);
        self.wall_origin.set(None);
        self.media_at_start.set(media_time_secs.max(0.0));
    }

    /// Soft hold (underrun / scrub / hidden) — same snap as [`Self::stop`].
    pub fn hold(&self) -> f64 {
        self.stop()
    }

    /// Eager teardown (clip switch) — Chrome caps concurrent `AudioContext`s.
    /// Idempotent; [`Drop`] also closes.
    pub fn close(&self) {
        let _ = self.context.close();
    }
}

impl Drop for PlaybackClock {
    fn drop(&mut self) {
        let _ = self.context.close();
    }
}
