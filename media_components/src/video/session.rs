//! Play-session clock state + measured display FPS.

/// Media-clock + UI throttle state for an active play session.
pub(super) struct PlaySession {
    pub(super) media_origin: f64,
    pub(super) wall_origin: f64,
    pub(super) last_ui_ms: f64,
    /// `loop_to_start` in flight — skip presents until origins reset.
    pub(super) restarting: bool,
    /// Media clock frozen while presentation queue is empty (underrun hold).
    pub(super) holding_for_buffer: bool,
    /// `start_media_clock` after hold in flight — do not stack resumes.
    pub(super) resuming_clock: bool,
    /// No-AAC PlayPrime: arm silent [`webmedia::VideoPlayer::arm_silent_play_clock`]
    /// on the first present rAF (not in the prime async turn).
    /// `mediabunny: media-player play` / `audioContextStartTime`.
    pub(super) pending_clock_start: bool,
}

/// Rolling window of canvas presents → measured display FPS.
pub(super) struct FpsMeter {
    presents: Vec<f64>,
}

impl FpsMeter {
    pub(super) fn new() -> Self {
        Self {
            presents: Vec::new(),
        }
    }

    pub(super) fn reset(&mut self) {
        self.presents.clear();
    }

    pub(super) fn note_present(&mut self) -> Option<f64> {
        let now = performance_now_ms()?;
        self.presents.push(now);
        let cutoff = now - 1000.0;
        self.presents.retain(|&t| t >= cutoff);
        if self.presents.len() < 2 {
            return None;
        }
        let first = *self.presents.first()?;
        let span_ms = now - first;
        if span_ms <= 0.0 {
            return None;
        }
        Some((self.presents.len() - 1) as f64 * 1000.0 / span_ms)
    }
}

pub(super) fn performance_now_ms() -> Option<f64> {
    web_sys::window()?.performance().map(|p| p.now())
}
