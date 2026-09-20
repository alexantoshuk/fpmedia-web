//! [`Timecode`] value: absolute frame count + [`FrameRate`].

use core::fmt;
use core::ops::{Add, Sub};

use crate::dropframe::{components_to_frames, frames_to_components};
use crate::error::TimecodeError;
use crate::format::format_timecode;
use crate::framerate::FrameRate;
use crate::parse::{parse_timecode, validate_components};

/// SMPTE timecode carrying its own frame rate.
///
/// Internally stored as an absolute frame count from `00:00:00:00` / `00:00:00;00`
/// at that rate (drop-frame adjusted when applicable).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Timecode {
    total_frames: i64,
    rate: FrameRate,
}

impl Timecode {
    /// Build from HH:MM:SS:FF fields.
    pub fn new(h: u8, m: u8, s: u8, f: u8, rate: FrameRate) -> Result<Self, TimecodeError> {
        validate_components(h, m, s, f, rate)?;
        Ok(Self {
            total_frames: components_to_frames(h, m, s, f, rate),
            rate,
        })
    }

    /// Absolute frame count at `rate` (from zero timecode).
    pub const fn from_frames(total_frames: i64, rate: FrameRate) -> Self {
        Self { total_frames, rate }
    }

    /// Parse `HH:MM:SS:FF` (NDF) or `HH:MM:SS;FF` (DF).
    pub fn parse(s: &str, rate: FrameRate) -> Result<Self, TimecodeError> {
        let (h, m, sec, f) = parse_timecode(s, rate)?;
        Ok(Self {
            total_frames: components_to_frames(h, m, sec, f, rate),
            rate,
        })
    }

    /// Timecode for presentation `frame_id` counting from zero.
    pub const fn from_zero(frame_id: u64, rate: FrameRate) -> Self {
        Self::from_frames(frame_id as i64, rate)
    }

    /// Timecode for presentation `frame_id` relative to an embedded start.
    pub fn from_start(frame_id: u64, start: Self) -> Self {
        Self {
            total_frames: start.total_frames + frame_id as i64,
            rate: start.rate,
        }
    }

    /// Presentation frame index from zero for this timecode.
    pub fn to_zero_frame(&self) -> Result<u64, TimecodeError> {
        if self.total_frames < 0 {
            return Err(TimecodeError::NegativeFrame);
        }
        Ok(self.total_frames as u64)
    }

    /// Presentation frame index relative to `start` (same rate required).
    pub fn to_frame_from_start(&self, start: Self) -> Result<u64, TimecodeError> {
        if self.rate != start.rate {
            return Err(TimecodeError::MismatchedRates);
        }
        let delta = self.total_frames - start.total_frames;
        if delta < 0 {
            return Err(TimecodeError::NegativeFrame);
        }
        Ok(delta as u64)
    }

    pub const fn total_frames(&self) -> i64 {
        self.total_frames
    }

    pub const fn rate(&self) -> FrameRate {
        self.rate
    }

    pub fn components(&self) -> (u8, u8, u8, u8) {
        frames_to_components(self.total_frames, self.rate)
    }

    pub fn hours(&self) -> u8 {
        self.components().0
    }

    pub fn minutes(&self) -> u8 {
        self.components().1
    }

    pub fn seconds(&self) -> u8 {
        self.components().2
    }

    pub fn frames(&self) -> u8 {
        self.components().3
    }

    /// Signed frame distance (`self - other`); rates must match.
    pub fn frame_diff(&self, other: Self) -> Result<i64, TimecodeError> {
        if self.rate != other.rate {
            return Err(TimecodeError::MismatchedRates);
        }
        Ok(self.total_frames - other.total_frames)
    }
}

impl fmt::Display for Timecode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let bytes = format_timecode(self.total_frames, self.rate);
        // Always valid ASCII digits + separators.
        let s = core::str::from_utf8(&bytes).unwrap_or("??:??:??:??");
        f.write_str(s)
    }
}

impl PartialOrd for Timecode {
    fn partial_cmp(&self, other: &Self) -> Option<core::cmp::Ordering> {
        if self.rate != other.rate {
            return None;
        }
        Some(self.total_frames.cmp(&other.total_frames))
    }
}

impl Add<i64> for Timecode {
    type Output = Self;

    fn add(self, frames: i64) -> Self {
        Self {
            total_frames: self.total_frames + frames,
            rate: self.rate,
        }
    }
}

impl Sub<i64> for Timecode {
    type Output = Self;

    fn sub(self, frames: i64) -> Self {
        Self {
            total_frames: self.total_frames - frames,
            rate: self.rate,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_to_string_and_back() {
        let tc = Timecode::from_frames(86400, FrameRate::Fps24);
        assert_eq!(tc.to_string(), "01:00:00:00");
        let parsed = Timecode::parse("01:00:00:00", FrameRate::Fps24).unwrap();
        assert_eq!(parsed.total_frames(), 86400);
    }

    #[test]
    fn zero_and_start_helpers() {
        let rate = FrameRate::Fps24;
        let start = Timecode::parse("10:00:00:00", rate).unwrap();
        let tape = Timecode::from_start(24, start);
        assert_eq!(tape.to_string(), "10:00:01:00");
        assert_eq!(tape.to_frame_from_start(start).unwrap(), 24);
        assert_eq!(Timecode::from_zero(24, rate).to_string(), "00:00:01:00");
        assert_eq!(Timecode::from_zero(24, rate).to_zero_frame().unwrap(), 24);
    }

    #[test]
    fn df_display() {
        let tc = Timecode::from_frames(1800, FrameRate::Fps29_97Df);
        assert_eq!(tc.to_string(), "00:01:00;02");
    }

    #[test]
    fn add_sub() {
        let tc = Timecode::from_frames(0, FrameRate::Fps24);
        assert_eq!((tc + 48).to_string(), "00:00:02:00");
        assert_eq!((tc + 48 - 24).to_string(), "00:00:01:00");
    }

    #[test]
    fn mismatched_rates() {
        let a = Timecode::from_frames(10, FrameRate::Fps24);
        let b = Timecode::from_frames(10, FrameRate::Fps30);
        assert!(a.frame_diff(b).is_err());
        assert!(a.partial_cmp(&b).is_none());
    }
}
