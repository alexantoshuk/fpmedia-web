//! Errors for timecode parse / arithmetic.

use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum TimecodeError {
    #[error("invalid timecode format (expected HH:MM:SS:FF or HH:MM:SS;FF)")]
    InvalidFormat,
    #[error("hours {0} out of range 0–23")]
    InvalidHours(u8),
    #[error("minutes {0} out of range 0–59")]
    InvalidMinutes(u8),
    #[error("seconds {0} out of range 0–59")]
    InvalidSeconds(u8),
    #[error("frames {0} out of range 0–{1}")]
    InvalidFrames(u8, u8),
    #[error("frame {0} is dropped at this minute in drop-frame timecode")]
    DroppedFrame(u8),
    #[error("';' separator requires a drop-frame rate")]
    InvalidDropFrameRate,
    #[error("cannot operate on timecodes with different frame rates")]
    MismatchedRates,
    #[error("resulting frame index is negative")]
    NegativeFrame,
}
