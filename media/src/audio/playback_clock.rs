//! Silent [`AudioContext`] media timeline (no AAC track).
//!
//! mediabunny `examples/media-player/media-player.ts` `getPlaybackTime` always
//! uses `audioContext.currentTime`, even when there is no audio track. Wall
//! `performance.now` was a local gap that fought Play smoothness on PCM-less
//! masters (ChG).

use std::cell::Cell;

use wasm_bindgen_futures::JsFuture;
use web_sys::{AudioContext, AudioContextOptions, AudioContextState};

use crate::error::MediaError;

/// Timeline driver when [`super::AudioOutput`] is absent.
pub struct PlaybackClock {
    context: AudioContext,
    ctx_origin: Cell<Option<f64>>,
    media_at_start: Cell<f64>,
    rate: Cell<f64>,
    running: Cell<bool>,
}

impl PlaybackClock {
    pub fn create() -> Result<Self, MediaError> {
        let opts = AudioContextOptions::new();
        // Default output rate — clock only; no buffers scheduled.
        let context = AudioContext::new_with_context_options(&opts).map_err(MediaError::from)?;
        Ok(Self {
            context,
            ctx_origin: Cell::new(None),
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
            && let Some(origin) = self.ctx_origin.get()
        {
            let elapsed = self.context.current_time() - origin;
            return self.media_at_start.get() + elapsed.max(0.0) * self.rate.get();
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

    /// Arm the context clock at `media_time_secs` (after [`Self::resume_context`]).
    pub fn start(&self, media_time_secs: f64) {
        self.media_at_start.set(media_time_secs.max(0.0));
        self.ctx_origin.set(Some(self.context.current_time()));
        self.running.set(true);
    }

    /// Snap media time and stop advancing.
    pub fn stop(&self) -> f64 {
        let t = self.media_time();
        self.running.set(false);
        self.ctx_origin.set(None);
        self.media_at_start.set(t);
        t
    }

    /// Snap to an explicit media time without running.
    pub fn snap(&self, media_time_secs: f64) {
        self.running.set(false);
        self.ctx_origin.set(None);
        self.media_at_start.set(media_time_secs.max(0.0));
    }

    /// Soft hold (underrun / scrub / hidden) — same snap as [`Self::stop`].
    pub fn hold(&self) -> f64 {
        self.stop()
    }
}
