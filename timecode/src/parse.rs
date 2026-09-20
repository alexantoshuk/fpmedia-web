//! Strict `HH:MM:SS:FF` / `HH:MM:SS;FF` parsing.

use crate::error::TimecodeError;
use crate::framerate::FrameRate;

/// Parse an 11-character SMPTE string into components.
pub fn parse_timecode(s: &str, rate: FrameRate) -> Result<(u8, u8, u8, u8), TimecodeError> {
    let bytes = s.as_bytes();
    if bytes.len() != 11 {
        return Err(TimecodeError::InvalidFormat);
    }

    let h = parse_two_digits(bytes[0], bytes[1])?;
    if bytes[2] != b':' {
        return Err(TimecodeError::InvalidFormat);
    }
    let m = parse_two_digits(bytes[3], bytes[4])?;
    if bytes[5] != b':' {
        return Err(TimecodeError::InvalidFormat);
    }
    let sec = parse_two_digits(bytes[6], bytes[7])?;

    let sep = bytes[8];
    if sep != b':' && sep != b';' {
        return Err(TimecodeError::InvalidFormat);
    }
    if sep == b';' && !rate.is_drop_frame() {
        return Err(TimecodeError::InvalidDropFrameRate);
    }

    let f = parse_two_digits(bytes[9], bytes[10])?;
    validate_components(h, m, sec, f, rate)?;
    Ok((h, m, sec, f))
}

pub(crate) fn validate_components(
    h: u8,
    m: u8,
    s: u8,
    f: u8,
    rate: FrameRate,
) -> Result<(), TimecodeError> {
    if h > 23 {
        return Err(TimecodeError::InvalidHours(h));
    }
    if m > 59 {
        return Err(TimecodeError::InvalidMinutes(m));
    }
    if s > 59 {
        return Err(TimecodeError::InvalidSeconds(s));
    }
    let max_frames = rate.nominal() as u8;
    if f >= max_frames {
        return Err(TimecodeError::InvalidFrames(
            f,
            max_frames.saturating_sub(1),
        ));
    }
    // Dropped numbers at non-10th minutes (e.g. 00:01:00;00 and ;01 @ 29.97 DF).
    if rate.is_drop_frame() {
        let drop = rate.drop_count() as u8;
        if !m.is_multiple_of(10) && f < drop {
            return Err(TimecodeError::DroppedFrame(f));
        }
    }
    Ok(())
}

fn parse_two_digits(tens: u8, ones: u8) -> Result<u8, TimecodeError> {
    if !tens.is_ascii_digit() || !ones.is_ascii_digit() {
        return Err(TimecodeError::InvalidFormat);
    }
    Ok((tens - b'0') * 10 + (ones - b'0'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_ndf() {
        assert_eq!(
            parse_timecode("01:23:45:12", FrameRate::Fps24),
            Ok((1, 23, 45, 12))
        );
    }

    #[test]
    fn valid_df() {
        assert_eq!(
            parse_timecode("01:23:45;12", FrameRate::Fps29_97Df),
            Ok((1, 23, 45, 12))
        );
    }

    #[test]
    fn semicolon_requires_df() {
        assert_eq!(
            parse_timecode("01:23:45;12", FrameRate::Fps24),
            Err(TimecodeError::InvalidDropFrameRate)
        );
    }

    #[test]
    fn rejects_dropped_frame_number() {
        assert_eq!(
            parse_timecode("00:01:00;00", FrameRate::Fps29_97Df),
            Err(TimecodeError::DroppedFrame(0))
        );
        assert_eq!(
            parse_timecode("00:01:00;01", FrameRate::Fps29_97Df),
            Err(TimecodeError::DroppedFrame(1))
        );
        assert!(parse_timecode("00:01:00;02", FrameRate::Fps29_97Df).is_ok());
        assert!(parse_timecode("00:10:00;00", FrameRate::Fps29_97Df).is_ok());
    }
}
