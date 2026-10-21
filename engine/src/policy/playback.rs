//! Pure play / EOF policy helpers (no WebCodecs / canvas).
//!
//! Extracted from [`crate::video_player`] so the EOF wall is unit-testable on
//! native. Callers wire results.
//!
//! Drain before End mirrors mediabunny `render`: keep drawing `nextFrame` until
//! the iterator returns nothing (`examples/media-player/media-player.ts`).

/// What to do when media time is at/past the timeline end.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EofWall {
    /// Iterator still has frames (or `nextFrame` waits) — keep presenting.
    DrainRemaining,
    /// Iterator exhausted, looping — host should restart at frame 0.
    NeedsLoop,
    /// Iterator exhausted, not looping — pause and End.
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
/// `past_end` = `media_time >= last_end`; `pending` = Play iterator not yet
/// exhausted or a decoded `nextFrame` still waits.
pub fn eof_wall_decision(past_end: bool, pending: bool, looping: bool) -> Option<EofWall> {
    if !past_end {
        return None;
    }
    if pending {
        return Some(EofWall::DrainRemaining);
    }
    Some(if looping {
        EofWall::NeedsLoop
    } else {
        EofWall::Ended
    })
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
        assert_eq!(eof_wall_decision(false, false, false), None);
    }

    #[test]
    fn eof_wall_drain_while_pending() {
        assert_eq!(
            eof_wall_decision(true, true, false),
            Some(EofWall::DrainRemaining)
        );
    }

    #[test]
    fn eof_wall_exhausted_ended_or_loop() {
        assert_eq!(eof_wall_decision(true, false, false), Some(EofWall::Ended));
        assert_eq!(
            eof_wall_decision(true, false, true),
            Some(EofWall::NeedsLoop)
        );
    }
}
