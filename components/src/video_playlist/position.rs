//! [`PlaylistPos`] + `[i]` readout / go-to parse.

use fpmedia_web::VideoMetadata;

use crate::helpers::{format_timecode_at_loose, last_frame_id, parse_position};

/// Playlist position: 0-based clip index + local presentation frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PlaylistPos {
    pub clip_index: u32,
    pub local_frame: u32,
}

impl PlaylistPos {
    pub fn new(clip_index: u32, local_frame: u32) -> Self {
        Self {
            clip_index,
            local_frame,
        }
    }

    /// 1-based playlist index for UI (`[i]`).
    pub fn display_index(self) -> u32 {
        self.clip_index.saturating_add(1)
    }
}

/// Frames readout: `[2] 10 / 100` (`local / frame_count`).
pub fn format_playlist_frames(pos: PlaylistPos, frame_count: u32) -> String {
    format!(
        "[{}] {} / {}",
        pos.display_index(),
        pos.local_frame,
        frame_count
    )
}

/// Timecode readout: `[1] 00:00:01:23 / 00:00:11:25` (current clip only).
pub fn format_playlist_timecode(pos: PlaylistPos, meta: &VideoMetadata) -> String {
    let last = last_frame_id(u32::try_from(meta.frame_count).unwrap_or(u32::MAX));
    let now = format_timecode_at_loose(meta, pos.local_frame);
    let end = format_timecode_at_loose(meta, last);
    format!("[{}] {now} / {end}", pos.display_index())
}

/// Split optional `[i]` prefix from a go-to string.
///
/// - `"[1] 00:00:01:23"` → `(Some(0), "00:00:01:23")` (0-based clip)
/// - `"10"` → `(None, "10")` — caller uses current clip
/// - Invalid / empty index → `None`
pub fn split_playlist_goto(raw: &str) -> Option<(Option<u32>, &str)> {
    let s = raw.trim();
    if s.is_empty() {
        return None;
    }
    if !s.starts_with('[') {
        return Some((None, s));
    }
    let close = s.find(']')?;
    let inside = s[1..close].trim();
    if inside.is_empty() {
        return None;
    }
    let one_based: u32 = inside.parse().ok()?;
    if one_based == 0 {
        return None;
    }
    let clip_index = one_based - 1;
    let rest = s[close + 1..].trim();
    if rest.is_empty() {
        return None;
    }
    Some((Some(clip_index), rest))
}

/// Parse go-to: `[i] <tc|frame>` or bare `<tc|frame>` for `current_clip`.
///
/// `meta_for_clip(clip_index)` must return metadata for the target clip.
pub fn parse_playlist_goto(
    raw: &str,
    current_clip: u32,
    mut meta_for_clip: impl FnMut(u32) -> Option<VideoMetadata>,
) -> Option<PlaylistPos> {
    let (clip_opt, rest) = split_playlist_goto(raw)?;
    let clip_index = clip_opt.unwrap_or(current_clip);
    let meta = meta_for_clip(clip_index)?;
    let local_frame = parse_position(&meta, rest)?;
    Some(PlaylistPos {
        clip_index,
        local_frame,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use fpmedia_web::{CodecConfig, GopProfile, VideoCodec, VideoMetadata};

    fn meta_24fps(frames: u64) -> VideoMetadata {
        VideoMetadata {
            width: 1920,
            height: 1080,
            timescale: 24,
            duration_ticks: frames,
            frame_count: frames,
            fps: Some(24.0),
            codec: CodecConfig {
                codec: VideoCodec::Avc,
                codec_string: None,
                description: Vec::new(),
            },
            gop_profile: GopProfile::ShortFixed { len: 12 },
            timecode_start: None,
        }
    }

    #[test]
    fn format_frames_and_timecode() {
        let pos = PlaylistPos::new(1, 10);
        assert_eq!(format_playlist_frames(pos, 100), "[2] 10 / 100");
        let meta = meta_24fps(240);
        let s = format_playlist_timecode(PlaylistPos::new(0, 24), &meta);
        assert_eq!(s, "[1] 00:00:01:00 / 00:00:09:23");
    }

    #[test]
    fn split_prefix() {
        assert_eq!(
            split_playlist_goto("[1] 00:00:01:23"),
            Some((Some(0), "00:00:01:23"))
        );
        assert_eq!(split_playlist_goto("[2] 10"), Some((Some(1), "10")));
        assert_eq!(split_playlist_goto("10"), Some((None, "10")));
        assert_eq!(split_playlist_goto("[0] 1"), None);
        assert_eq!(split_playlist_goto("[1]"), None);
        assert_eq!(split_playlist_goto(""), None);
    }

    #[test]
    fn parse_goto_with_and_without_prefix() {
        let metas = [meta_24fps(100), meta_24fps(200)];
        let resolve = |i: u32| metas.get(i as usize).cloned();

        assert_eq!(
            parse_playlist_goto("[2] 10", 0, resolve),
            Some(PlaylistPos::new(1, 10))
        );
        assert_eq!(
            parse_playlist_goto("12", 0, resolve),
            Some(PlaylistPos::new(0, 12))
        );
        assert_eq!(
            parse_playlist_goto("[1] 00:00:01:00", 1, resolve),
            Some(PlaylistPos::new(0, 24))
        );
        assert!(parse_playlist_goto("[9] 1", 0, resolve).is_none());
    }
}
