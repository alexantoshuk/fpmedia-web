//! Drop-frame / non-drop frame ↔ component conversion.
//!
//! Drop-frame arithmetic follows David Heidelberger’s formulation of SMPTE ST 12-1
//! (same approach as eoyilmaz/timecode and OpenCinemaC vtc).

use crate::framerate::FrameRate;

/// Decompose an absolute frame count into HH:MM:SS:FF components.
pub fn frames_to_components(total_frames: i64, rate: FrameRate) -> (u8, u8, u8, u8) {
    let nom = rate.nominal();

    if !rate.is_drop_frame() {
        return non_drop_decompose(total_frames, nom);
    }

    let drop = rate.drop_count();
    let frames_per_min = nom * 60 - drop;
    let frames_per_10min = frames_per_min * 10 + drop;

    let abs_frames = wrap_day(total_frames, frames_per_10min as u64 * 6 * 24);

    let first_minute_frames = (nom * 60) as u64;

    let ten_min_blocks = abs_frames / frames_per_10min as u64;
    let remainder = abs_frames % frames_per_10min as u64;

    let (minutes_in_block, frames_in_block) = if remainder < first_minute_frames {
        (0u64, remainder)
    } else {
        let adjusted = remainder - first_minute_frames;
        let mins = 1 + adjusted / frames_per_min as u64;
        let fr = adjusted % frames_per_min as u64 + drop as u64;
        (mins, fr)
    };

    let total_minutes = ten_min_blocks * 10 + minutes_in_block;
    let hours = (total_minutes / 60) % 24;
    let minutes = total_minutes % 60;
    let seconds = frames_in_block / nom as u64;
    let frames = frames_in_block % nom as u64;

    (hours as u8, minutes as u8, seconds as u8, frames as u8)
}

/// Convert HH:MM:SS:FF components to an absolute frame count.
pub fn components_to_frames(h: u8, m: u8, s: u8, f: u8, rate: FrameRate) -> i64 {
    let nom = rate.nominal();

    let nominal_frames = h as i64 * 3600 * nom as i64
        + m as i64 * 60 * nom as i64
        + s as i64 * nom as i64
        + f as i64;

    if !rate.is_drop_frame() {
        return nominal_frames;
    }

    let drop = rate.drop_count() as i64;
    let total_minutes = h as i64 * 60 + m as i64;
    let drop_adjustment = drop * (total_minutes - total_minutes / 10);

    nominal_frames - drop_adjustment
}

fn non_drop_decompose(total_frames: i64, nom: u32) -> (u8, u8, u8, u8) {
    let fps = nom as u64;
    let abs_frames = wrap_day(total_frames, fps * 86400);

    let frames = abs_frames % fps;
    let total_secs = abs_frames / fps;
    let seconds = total_secs % 60;
    let total_mins = total_secs / 60;
    let minutes = total_mins % 60;
    let hours = (total_mins / 60) % 24;

    (hours as u8, minutes as u8, seconds as u8, frames as u8)
}

fn wrap_day(total_frames: i64, day_frames: u64) -> u64 {
    let day = day_frames as i64;
    if total_frames < 0 {
        ((total_frames % day) + day) as u64
    } else {
        total_frames as u64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_drop_roundtrip() {
        for frames in [0, 1, 12, 24, 1439, 86400, 172800, 2_073_599] {
            let (h, m, s, f) = frames_to_components(frames, FrameRate::Fps24);
            assert_eq!(
                components_to_frames(h, m, s, f, FrameRate::Fps24),
                frames
            );
        }
    }

    #[test]
    fn drop_frame_minute_boundary_29_97() {
        assert_eq!(
            frames_to_components(1799, FrameRate::Fps29_97Df),
            (0, 0, 59, 29)
        );
        assert_eq!(
            frames_to_components(1800, FrameRate::Fps29_97Df),
            (0, 1, 0, 2)
        );
    }

    #[test]
    fn drop_frame_10th_minute_29_97() {
        let ten_min_frames = 17_982i64;
        assert_eq!(
            frames_to_components(ten_min_frames, FrameRate::Fps29_97Df),
            (0, 10, 0, 0)
        );
    }

    #[test]
    fn drop_frame_roundtrip_29_97() {
        for frames in [0, 1, 2, 1798, 1799, 1800, 1801, 1802, 17_982, 17_983, 107_892] {
            let (h, m, s, f) = frames_to_components(frames, FrameRate::Fps29_97Df);
            let back = components_to_frames(h, m, s, f, FrameRate::Fps29_97Df);
            assert_eq!(
                back, frames,
                "roundtrip failed at frame {frames}: ({h}:{m}:{s};{f})"
            );
        }
    }

    #[test]
    fn drop_frame_59_94_minute_boundary() {
        assert_eq!(
            frames_to_components(3599, FrameRate::Fps59_94Df),
            (0, 0, 59, 59)
        );
        assert_eq!(
            frames_to_components(3600, FrameRate::Fps59_94Df),
            (0, 1, 0, 4)
        );
    }

    #[test]
    fn drop_frame_59_94_roundtrip() {
        for frames in [0, 4, 3599, 3600, 3604, 35_964, 35_968] {
            let (h, m, s, f) = frames_to_components(frames, FrameRate::Fps59_94Df);
            let back = components_to_frames(h, m, s, f, FrameRate::Fps59_94Df);
            assert_eq!(back, frames, "59.94DF roundtrip failed at frame {frames}");
        }
    }

    #[test]
    fn negative_wraps_to_24h() {
        let (h, _, _, _) = frames_to_components(-24, FrameRate::Fps24);
        assert_eq!(h, 23);
    }
}
