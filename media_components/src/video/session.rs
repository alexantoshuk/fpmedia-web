//! Measured display FPS for the demo pipeline panel.

use webmedia::timer::now_ms;

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
        let now = now_ms()?;
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
