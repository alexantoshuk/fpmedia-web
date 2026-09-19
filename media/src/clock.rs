//! Shared media timeline for continuous play.
//!
//! Until an audio track drives playback, the clock is wall time
//! ([`performance.now`](https://developer.mozilla.org/en-US/docs/Web/API/Performance/now)).
//! With audio, the host/engine uses [`AudioContext.currentTime`](https://developer.mozilla.org/en-US/docs/Web/API/BaseAudioContext/currentTime)
//! (mediabunny `examples/media-player` `getPlaybackTime`).
//!
//! Playback rate multiplies driver elapsed time. Public API:
//! - forward review/shuttle: `0.25…8`
//! - reverse JKL: `−8…−1` (full reverse review `−0.25…−1.75` later)
//!
//! `|rate| >= `[`PLAYBACK_RATE_SHUTTLE_MIN`] → shuttle policy (wall clock, audio off,
//! lighter present queue). Any reverse also uses wall + audio off until reverse
//! review ships. Reverse present = decode forward per frame, queue toward the past
//! (mediabunny: no reverse iterator — GOP/key restart; `#350` encode-reverse).
//! mediabunny has no rate API — base formula stays `origin + elapsed`; we add `* rate`.

/// Minimum forward playback rate (Frame.io review floor).
pub const PLAYBACK_RATE_MIN: f64 = 0.25;
/// Maximum |rate| (JKL shuttle ceiling).
pub const PLAYBACK_RATE_MAX: f64 = 8.0;
/// Frame.io review slider ceiling (UI hint; API still allows up to [`PLAYBACK_RATE_MAX`]).
pub const PLAYBACK_RATE_REVIEW_MAX: f64 = 1.75;
/// Minimum |reverse| rate (JKL −1×).
pub const PLAYBACK_RATE_REVERSE_MIN: f64 = 1.0;
/// |rate| at or above this uses shuttle decode/present/audio policy.
pub const PLAYBACK_RATE_SHUTTLE_MIN: f64 = 2.0;
/// Snap step for review + shuttle rates.
pub const PLAYBACK_RATE_STEP: f64 = 0.05;

/// Clamp + snap. Forward `0.25…8`; reverse `−8…−1`.
pub fn normalize_playback_rate(rate: f64) -> f64 {
    if !rate.is_finite() || rate == 0.0 {
        return 1.0;
    }
    if rate < 0.0 {
        let mag = rate
            .abs()
            .clamp(PLAYBACK_RATE_REVERSE_MIN, PLAYBACK_RATE_MAX);
        let snapped = (mag / PLAYBACK_RATE_STEP).round() * PLAYBACK_RATE_STEP;
        return -snapped.clamp(PLAYBACK_RATE_REVERSE_MIN, PLAYBACK_RATE_MAX);
    }
    let clamped = rate.clamp(PLAYBACK_RATE_MIN, PLAYBACK_RATE_MAX);
    let snapped = (clamped / PLAYBACK_RATE_STEP).round() * PLAYBACK_RATE_STEP;
    snapped.clamp(PLAYBACK_RATE_MIN, PLAYBACK_RATE_MAX)
}

/// True when |rate| should use shuttle policy (audio off, wall clock, lighter queue).
pub fn is_shuttle_rate(rate: f64) -> bool {
    normalize_playback_rate(rate).abs() >= PLAYBACK_RATE_SHUTTLE_MIN
}

/// True when rate plays toward the past (J shuttle / −1×).
pub fn is_reverse_rate(rate: f64) -> bool {
    normalize_playback_rate(rate) < 0.0
}

/// Wall clock + audio off: shuttle (`|rate| >= 2`) or any reverse (until reverse review).
pub fn is_wall_clock_rate(rate: f64) -> bool {
    is_shuttle_rate(rate) || is_reverse_rate(rate)
}

/// Playback media clock origins. Pure Rust — drivers (wall / audio) are passed in.
#[derive(Clone, Debug)]
pub struct MediaClock {
    /// Media time (seconds) at the last [`Self::start`] / pause snap.
    media_origin: f64,
    /// Media seconds per driver second (1.0 = realtime; negative = reverse).
    rate: f64,
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
            rate: 1.0,
            playing: false,
        }
    }

    pub fn media_origin(&self) -> f64 {
        self.media_origin
    }

    pub fn playback_rate(&self) -> f64 {
        self.rate
    }

    pub fn is_shuttle(&self) -> bool {
        is_shuttle_rate(self.rate)
    }

    pub fn is_reverse(&self) -> bool {
        is_reverse_rate(self.rate)
    }

    /// Set playback rate (already normalized by callers that need Frame.io snap).
    pub fn set_playback_rate(&mut self, rate: f64) {
        self.rate = normalize_playback_rate(rate);
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
    ///   driver (wall delta or `ctx.currentTime - ctx_origin`), scaled by rate
    ///   (negative rate rewinds).
    /// - Paused → [`Self::media_origin`].
    pub fn media_time(&self, driver_elapsed: Option<f64>) -> f64 {
        if self.playing {
            self.media_origin + driver_elapsed.unwrap_or(0.0).max(0.0) * self.rate
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

    #[test]
    fn rate_scales_elapsed() {
        let mut c = MediaClock::new();
        c.set_playback_rate(0.5);
        c.start(1.0);
        assert!((c.media_time(Some(2.0)) - 2.0).abs() < 1e-9);
        c.set_playback_rate(2.0);
        assert!((c.playback_rate() - 2.0).abs() < 1e-9);
        assert!(c.is_shuttle());
        c.start(0.0);
        assert!((c.media_time(Some(1.0)) - 2.0).abs() < 1e-9);
        c.set_playback_rate(8.0);
        assert!((c.playback_rate() - 8.0).abs() < 1e-9);
    }

    #[test]
    fn reverse_rate_rewinds() {
        let mut c = MediaClock::new();
        c.set_playback_rate(-2.0);
        assert!(c.is_reverse());
        assert!(c.is_shuttle());
        c.start(10.0);
        assert!((c.media_time(Some(1.0)) - 8.0).abs() < 1e-9);
        c.set_playback_rate(-1.0);
        assert!((c.playback_rate() - -1.0).abs() < 1e-9);
        assert!(c.is_reverse());
        assert!(!c.is_shuttle());
        assert!(is_wall_clock_rate(-1.0));
        c.set_playback_rate(-0.5); // snaps to reverse floor (−1×)
        assert!((c.playback_rate() - -1.0).abs() < 1e-9);
    }

    #[test]
    fn normalize_snaps_to_step() {
        assert!((normalize_playback_rate(1.0) - 1.0).abs() < 1e-9);
        assert!((normalize_playback_rate(1.02) - 1.0).abs() < 1e-9);
        assert!((normalize_playback_rate(1.03) - 1.05).abs() < 1e-9);
        assert!((normalize_playback_rate(0.1) - 0.25).abs() < 1e-9);
        assert!((normalize_playback_rate(9.0) - 8.0).abs() < 1e-9);
        assert!((normalize_playback_rate(f64::NAN) - 1.0).abs() < 1e-9);
        assert!((normalize_playback_rate(-3.0) - -3.0).abs() < 1e-9);
        assert!((normalize_playback_rate(-1.0) - -1.0).abs() < 1e-9);
        assert!((normalize_playback_rate(-9.0) - -8.0).abs() < 1e-9);
        assert!(is_shuttle_rate(2.0));
        assert!(is_shuttle_rate(-2.0));
        assert!(!is_shuttle_rate(-1.0));
        assert!(!is_shuttle_rate(1.75));
        assert!(is_wall_clock_rate(-1.0));
        assert!(is_reverse_rate(-2.0));
        assert!(is_reverse_rate(-1.0));
        assert!(!is_reverse_rate(2.0));
    }
}
