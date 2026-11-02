//! Pure play / EOF policy helpers (no WebCodecs / canvas).
//!
//! Extracted from [`crate::video_player`] so the EOF wall is unit-testable on
//! native. Callers wire results.
//!
//! Drain before End mirrors mediabunny `render`: keep drawing `nextFrame` until
//! the iterator returns nothing (`examples/media-player/media-player.ts`).
//!
//! **LoopZone** — half-open wrap `[in, out)` when looping (**no equivalent**;
//! MB media-player has no loop). Full axis = Remotion-shaped seam at 0.

use crate::types::FrameId;

/// What to do when media time is at/past the timeline end.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EofWall {
    /// Iterator still has frames (or `nextFrame` waits) — keep presenting.
    DrainRemaining,
    /// Iterator exhausted, looping — host should restart at loop-in.
    NeedsLoop,
    /// Iterator exhausted, not looping — pause and End.
    Ended,
}

/// Half-open presentation wrap range `[in_frame, out_frame)` while looping.
///
/// Default / full file: `in_frame = 0`, `out_frame = frame_count`.
/// **no equivalent** (loop zone local). UX: `product.md` § Loop zone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LoopZone {
    /// Inclusive start (presentation [`FrameId`]).
    pub in_frame: FrameId,
    /// Exclusive end (`out_frame > in_frame` when `frame_count > 0`).
    pub out_frame: FrameId,
}

impl LoopZone {
    /// Full-axis zone for `frame_count` samples (`[0, n)`). Empty when `n == 0`.
    pub fn full(frame_count: FrameId) -> Self {
        Self {
            in_frame: 0,
            out_frame: frame_count,
        }
    }

    /// Clamp / order so `out > in` within `[0, frame_count]` when possible.
    pub fn normalize(in_frame: FrameId, out_frame: FrameId, frame_count: FrameId) -> Self {
        if frame_count == 0 {
            return Self::full(0);
        }
        let in_f = in_frame.min(frame_count.saturating_sub(1));
        let mut out_f = out_frame.min(frame_count).max(in_f.saturating_add(1));
        if out_f <= in_f {
            out_f = (in_f + 1).min(frame_count);
        }
        if out_f <= in_f {
            // Single-frame clip: zone is the whole file.
            return Self::full(frame_count);
        }
        Self {
            in_frame: in_f,
            out_frame: out_f,
        }
    }

    /// True when this matches the full timeline.
    pub fn is_full(self, frame_count: FrameId) -> bool {
        self.in_frame == 0 && self.out_frame == frame_count
    }

    /// `frame` is inside `[in, out)`.
    pub fn contains(self, frame: FrameId) -> bool {
        frame >= self.in_frame && frame < self.out_frame
    }

    /// Playhead should snap to `in_frame` before Play while looping.
    pub fn needs_play_snap(self, frame: FrameId) -> bool {
        !self.contains(frame)
    }
}

/// fMP4 warm-open: while the index is shorter than provisional `frame_count`,
/// use `max(last_sample_end, metadata_duration)` so EOF is not early.
pub fn eof_timeline_end(
    sample_end: f64,
    provisional_duration: f64,
    indexed_len: u64,
    frame_count: u64,
) -> f64 {
    if indexed_len < frame_count {
        sample_end.max(provisional_duration)
    } else {
        sample_end
    }
}

/// Decision at/past timeline / zone end (before drawing due frames when draining).
///
/// `past_end` = `media_time >= wall_end`; `pending` = Play iterator not yet
/// exhausted or a decoded `nextFrame` still waits.
///
/// `drain_pending`: full-axis EOF may still have a last `nextFrame` to paint.
/// Mid [`LoopZone`] wall must **not** drain — the iterator still holds post-zone
/// samples, so pending would stay true forever and `out` would be ignored.
pub fn eof_wall_decision(
    past_end: bool,
    pending: bool,
    looping: bool,
    drain_pending: bool,
) -> Option<EofWall> {
    if !past_end {
        return None;
    }
    if pending && drain_pending {
        return Some(EofWall::DrainRemaining);
    }
    Some(if looping {
        EofWall::NeedsLoop
    } else {
        EofWall::Ended
    })
}

/// Near-EOF window for loop-origin byte prewarm (Remotion `prewarmIteratorForLooping`
/// shape; mediabunny `examples/media-player` has no loop).
pub const LOOP_ORIGIN_PREWARM_SECS: f64 = 1.0;

/// True while media time is inside `(last_end - lead, last_end]` (still before EOF).
pub fn should_prewarm_loop_origin(media_time: f64, last_end: f64, lead_secs: f64) -> bool {
    if !(media_time.is_finite() && last_end.is_finite() && lead_secs.is_finite())
        || lead_secs <= 0.0
    {
        return false;
    }
    let remain = last_end - media_time;
    remain > 0.0 && remain <= lead_secs
}

/// Wall end for a LoopZone: sample end of `out_frame - 1` when the zone is a
/// proper subset; otherwise the full-timeline `full_last_end`.
///
/// `out_minus_one_sample_end` = composition end of frame `out - 1` (required when
/// `!zone.is_full`). `full_last_end` = [`eof_timeline_end`] / last sample end.
pub fn loop_zone_wall_end(
    zone: LoopZone,
    frame_count: FrameId,
    out_minus_one_sample_end: Option<f64>,
    full_last_end: f64,
) -> f64 {
    if zone.is_full(frame_count) || frame_count == 0 {
        return full_last_end;
    }
    out_minus_one_sample_end.unwrap_or(full_last_end)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eof_timeline_uses_provisional_while_index_short() {
        assert_eq!(eof_timeline_end(1.0, 10.0, 5, 100), 10.0);
        assert_eq!(eof_timeline_end(12.0, 10.0, 100, 100), 12.0);
    }

    #[test]
    fn eof_wall_not_past_end() {
        assert_eq!(eof_wall_decision(false, false, false, true), None);
    }

    #[test]
    fn eof_wall_drain_while_pending() {
        assert_eq!(
            eof_wall_decision(true, true, false, true),
            Some(EofWall::DrainRemaining)
        );
    }

    #[test]
    fn eof_wall_exhausted_ended_or_loop() {
        assert_eq!(
            eof_wall_decision(true, false, false, true),
            Some(EofWall::Ended)
        );
        assert_eq!(
            eof_wall_decision(true, false, true, true),
            Some(EofWall::NeedsLoop)
        );
    }

    #[test]
    fn mid_zone_wall_loops_even_when_pending() {
        // Post-zone samples still queued — must not DrainRemaining.
        assert_eq!(
            eof_wall_decision(true, true, true, false),
            Some(EofWall::NeedsLoop)
        );
    }

    #[test]
    fn loop_origin_prewarm_window() {
        let lead = LOOP_ORIGIN_PREWARM_SECS;
        assert!(!should_prewarm_loop_origin(18.0, 20.0, lead));
        assert!(should_prewarm_loop_origin(19.2, 20.0, lead));
        assert!(!should_prewarm_loop_origin(20.0, 20.0, lead));
        assert!(!should_prewarm_loop_origin(21.0, 20.0, lead));
    }

    #[test]
    fn loop_zone_normalize_and_contains() {
        let z = LoopZone::normalize(10, 5, 100);
        assert_eq!(z, LoopZone::normalize(10, 11, 100));
        assert!(z.contains(10));
        assert!(!z.contains(11));
        assert!(z.needs_play_snap(0));
        assert!(!LoopZone::full(100).needs_play_snap(50));
        assert!(LoopZone::full(100).is_full(100));
        assert!(!LoopZone::normalize(2, 8, 100).is_full(100));
    }

    #[test]
    fn loop_zone_wall_uses_subset_end() {
        let z = LoopZone::normalize(10, 40, 100);
        assert_eq!(loop_zone_wall_end(z, 100, Some(1.5), 10.0), 1.5);
        assert_eq!(
            loop_zone_wall_end(LoopZone::full(100), 100, Some(1.5), 10.0),
            10.0
        );
    }
}
