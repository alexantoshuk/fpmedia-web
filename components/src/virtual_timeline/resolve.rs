//! Global FrameId → backend aim (Single identity / Playlist Σ map).
//!
//! **no equivalent** (composition local).

use crate::video_playlist::{PlaylistModel, PlaylistPos};

/// Aim resolved onto a concrete decode surface (slot / single player).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BackendAim {
    /// Virtual timeline unit (Σ frames for playlist; identity for single).
    pub global: u32,
    /// Local presentation FrameId on the player that will decode/present.
    pub local: u32,
    /// Playlist clip index; `0` for single-clip backends.
    pub clip_index: u32,
}

impl BackendAim {
    pub fn single(global: u32) -> Self {
        Self {
            global,
            local: global,
            clip_index: 0,
        }
    }

    pub fn from_playlist_pos(global: u32, pos: PlaylistPos) -> Self {
        Self {
            global,
            local: pos.local_frame,
            clip_index: pos.clip_index,
        }
    }
}

/// Single-clip: global FrameId == local (clamp to `len` when `len > 0`).
pub fn resolve_single(global: u32, len: u32) -> Option<BackendAim> {
    if len == 0 {
        return None;
    }
    let local = global.min(len.saturating_sub(1));
    Some(BackendAim::single(local))
}

/// Playlist: map global → clip + local via [`PlaylistModel`].
pub fn resolve_playlist(model: &PlaylistModel, global: u32) -> Option<BackendAim> {
    let pos = model.at_global_frame(global)?;
    Some(BackendAim::from_playlist_pos(global, pos))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::video_playlist::PlaylistClip;

    #[test]
    fn resolve_single_identity() {
        assert_eq!(
            resolve_single(12, 100),
            Some(BackendAim {
                global: 12,
                local: 12,
                clip_index: 0
            })
        );
        assert_eq!(resolve_single(0, 0), None);
        assert_eq!(
            resolve_single(99, 50),
            Some(BackendAim {
                global: 49,
                local: 49,
                clip_index: 0
            })
        );
    }

    #[test]
    fn resolve_playlist_across_cut() {
        let model = PlaylistModel::new(vec![
            PlaylistClip {
                frame_count: 10,
                duration_secs: 1.0,
                fps: Some(10.0),
            },
            PlaylistClip {
                frame_count: 20,
                duration_secs: 2.0,
                fps: Some(10.0),
            },
        ]);
        let at_cut = resolve_playlist(&model, 10).expect("second clip start");
        assert_eq!(at_cut.clip_index, 1);
        assert_eq!(at_cut.local, 0);
        assert_eq!(at_cut.global, 10);
        let last_first = resolve_playlist(&model, 9).expect("end of first");
        assert_eq!(last_first.clip_index, 0);
        assert_eq!(last_first.local, 9);
    }
}
