//! Pure play / EOF / near-EOF policy helpers (no WebCodecs / canvas).
//!
//! Extracted from [`crate::video_player`] so underrun / EOF wall / loop-prepare
//! math is unit-testable on native. Behaviour unchanged — callers wire results.
//!
//! EOF wall mid-timeline snap: `mediabunny: no equivalent` (no rate API).
//! Present-queue drain before End mirrors sink consumer emptying before stop.

use crate::types::FrameId;

/// What to do when media time is at/past the timeline end.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EofWall {
    /// Presentation still has frames — keep presenting due frames.
    DrainRemaining,
    /// Clock raced past duration while playhead is not on the last frame
    /// (shuttle / wall-clock rates). Snap clock; do not End.
    SnapClockContinue,
    /// At last frame, looping — host should restart.
    NeedsLoop,
    /// At last frame, not looping — pause and End.
    Ended,
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

/// Decision at/past timeline end (before drawing due frames when draining).
///
/// `past_end` = `media_time >= last_end`. Empty presentation + playhead before
/// last → snap (shuttle race); at last → loop or end.
pub fn eof_wall_decision(
    past_end: bool,
    presentation_len: usize,
    playhead: FrameId,
    last_id: FrameId,
    looping: bool,
) -> Option<EofWall> {
    if !past_end {
        return None;
    }
    if presentation_len > 0 {
        return Some(EofWall::DrainRemaining);
    }
    if playhead < last_id {
        return Some(EofWall::SnapClockContinue);
    }
    Some(if looping {
        EofWall::NeedsLoop
    } else {
        EofWall::Ended
    })
}

/// True when remaining frames are within rate-scaled `lead_secs` of EOF.
pub fn near_eof_by_frames(frames_left: f64, lead_secs: f64, fps: f64, rate: f64) -> bool {
    if lead_secs <= 0.0 {
        return false;
    }
    let rate = rate.max(0.25);
    let fps = if fps.is_finite() && fps > 0.0 {
        fps
    } else {
        24.0
    };
    let lead_frames = (lead_secs * fps * rate).ceil().max(1.0);
    frames_left <= lead_frames
}

/// Frames to keep queued near EOF for loop-prime / drain (clamped).
pub fn loop_drain_frames(budget_secs: f64, fps: f64, rate: f64, floor: usize, cap: usize) -> usize {
    let rate = rate.max(0.25);
    let fps = if fps.is_finite() && fps > 0.0 {
        fps
    } else {
        24.0
    };
    let needed = (budget_secs * fps * rate).ceil() as usize;
    needed.clamp(floor, cap)
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
        assert_eq!(eof_wall_decision(false, 0, 0, 99, false), None);
    }

    #[test]
    fn eof_wall_drain_when_queue_nonempty() {
        assert_eq!(
            eof_wall_decision(true, 3, 50, 99, false),
            Some(EofWall::DrainRemaining)
        );
    }

    #[test]
    fn eof_wall_shuttle_snap_mid_timeline() {
        assert_eq!(
            eof_wall_decision(true, 0, 50, 99, false),
            Some(EofWall::SnapClockContinue)
        );
    }

    #[test]
    fn eof_wall_at_last_ended_or_loop() {
        assert_eq!(
            eof_wall_decision(true, 0, 99, 99, false),
            Some(EofWall::Ended)
        );
        assert_eq!(
            eof_wall_decision(true, 0, 99, 99, true),
            Some(EofWall::NeedsLoop)
        );
    }

    #[test]
    fn near_eof_respects_lead_and_rate() {
        assert!(!near_eof_by_frames(100.0, 0.0, 24.0, 1.0));
        // lead 1s @ 24fps → 24 frames
        assert!(near_eof_by_frames(24.0, 1.0, 24.0, 1.0));
        assert!(!near_eof_by_frames(25.0, 1.0, 24.0, 1.0));
        // rate 2 → 48 lead frames
        assert!(near_eof_by_frames(48.0, 1.0, 24.0, 2.0));
        assert!(!near_eof_by_frames(49.0, 1.0, 24.0, 2.0));
    }

    #[test]
    fn loop_drain_clamps_to_floor_and_cap() {
        assert_eq!(loop_drain_frames(0.4, 24.0, 1.0, 8, 64), 10); // 0.4*24=9.6→10
        assert_eq!(loop_drain_frames(0.1, 24.0, 1.0, 8, 64), 8); // below floor
        assert_eq!(loop_drain_frames(10.0, 24.0, 1.0, 8, 64), 64); // above cap
    }
}
