//! Format absolute frame counts as SMPTE strings.

use crate::dropframe::frames_to_components;
use crate::framerate::FrameRate;

/// Format as fixed 11-byte ASCII (`HH:MM:SS:FF` or `HH:MM:SS;FF`).
pub fn format_timecode(total_frames: i64, rate: FrameRate) -> [u8; 11] {
    let (h, m, s, f) = frames_to_components(total_frames, rate);
    let sep = if rate.is_drop_frame() { b';' } else { b':' };

    [
        b'0' + h / 10,
        b'0' + h % 10,
        b':',
        b'0' + m / 10,
        b'0' + m % 10,
        b':',
        b'0' + s / 10,
        b'0' + s % 10,
        sep,
        b'0' + f / 10,
        b'0' + f % 10,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fmt(frames: i64, rate: FrameRate) -> String {
        let bytes = format_timecode(frames, rate);
        String::from_utf8(bytes.to_vec()).unwrap()
    }

    #[test]
    fn zero_and_hour() {
        assert_eq!(fmt(0, FrameRate::Fps24), "00:00:00:00");
        assert_eq!(fmt(86400, FrameRate::Fps24), "01:00:00:00");
    }

    #[test]
    fn drop_uses_semicolon() {
        assert_eq!(fmt(1800, FrameRate::Fps29_97Df), "00:01:00;02");
    }
}
