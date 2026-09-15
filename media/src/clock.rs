//! Shared media timeline for continuous play.
//!
//! Until an audio track drives playback, the clock is wall time
//! ([`performance.now`](https://developer.mozilla.org/en-US/docs/Web/API/Performance/now)).
//! With audio, the host/engine uses [`AudioContext.currentTime`](https://developer.mozilla.org/en-US/docs/Web/API/BaseAudioContext/currentTime)
//! (mediabunny `examples/media-player` `getPlaybackTime`).

/// Playback media clock origins. Pure Rust — drivers (wall / audio) are passed in.
#[derive(Clone, Debug)]
pub struct MediaClock {
    /// Media time (seconds) at the last [`Self::start`] / pause snap.
    media_origin: f64,
    playing: bool,
}

impl Default for MediaClock {
    fn default() -> Self {
        Self::new()
    }
}

impl MediaClock {
    pub fn new() -> Self {
        Self {
            media_origin: 0.0,
            playing: false,
        }
    }

    pub fn media_origin(&self) -> f64 {
        self.media_origin
    }

    pub fn is_playing(&self) -> bool {
        self.playing
    }

    /// Snap to a paused media time (seek / pause / open).
    pub fn snap(&mut self, media_time_secs: f64) {
        self.media_origin = media_time_secs.max(0.0);
        self.playing = false;
    }

    /// Begin continuous play from `media_time_secs`. Caller records wall or
    /// `AudioContext` start time separately and passes it to [`Self::media_time`].
    pub fn start(&mut self, media_time_secs: f64) {
        self.media_origin = media_time_secs.max(0.0);
        self.playing = true;
    }

    /// Current media time.
    ///
    /// - Playing + `driver_elapsed` = seconds since [`Self::start`] on the active
    ///   driver (wall delta or `ctx.currentTime - ctx_origin`).
    /// - Paused → [`Self::media_origin`].
    pub fn media_time(&self, driver_elapsed: Option<f64>) -> f64 {
        if self.playing {
            self.media_origin + driver_elapsed.unwrap_or(0.0).max(0.0)
        } else {
            self.media_origin
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wall_play_advances() {
        let mut c = MediaClock::new();
        c.start(1.5);
        assert!((c.media_time(Some(0.5)) - 2.0).abs() < 1e-9);
        c.snap(c.media_time(Some(0.5)));
        assert!(!c.is_playing());
        assert!((c.media_time(None) - 2.0).abs() < 1e-9);
    }
}
