//! Global FrameId → backend aim (Single identity / Playlist Σ map).
//!
//! **no equivalent** (composition local).

use fpmedia_web::FrameId;

use crate::frame_axis::{ClipFrame, GlobalFrame};
use crate::video_playlist::{PlaylistModel, PlaylistPos};

/// Aim resolved onto a concrete decode surface (slot / single player).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BackendAim {
    /// Virtual timeline unit (Σ frames for playlist; identity for single).
    pub global: GlobalFrame,
    /// Position inside the clip on the player that will decode/present
    /// (hand to the engine via [ClipFrame::source]).
    pub frame: ClipFrame,
    /// Playlist clip index; `0` for single-clip backends.
    pub clip_index: u32,
}

impl BackendAim {
    pub fn single(global: GlobalFrame) -> Self {
        Self {
            global,
            frame: ClipFrame::new(global.get()),
            clip_index: 0,
        }
    }

    pub fn from_playlist_pos(global: GlobalFrame, pos: PlaylistPos) -> Self {
        Self {
            global,
            frame: pos.frame,
            clip_index: pos.clip_index,
        }
    }
}

/// Single-clip: global FrameId == local (clamp to `len` when `len > 0`).
pub fn resolve_single(global: GlobalFrame, len: FrameId) -> Option<BackendAim> {
    if len == 0 {
        return None;
    }
    let clamped = GlobalFrame::new(global.get().min(len.saturating_sub(1)));
    Some(BackendAim::single(clamped))
}

/// Playlist: map global → clip + local via [`PlaylistModel`].
pub fn resolve_playlist(model: &PlaylistModel, global: GlobalFrame) -> Option<BackendAim> {
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
            resolve_single(GlobalFrame::new(12), 100),
            Some(BackendAim {
                global: GlobalFrame::new(12),
                frame: ClipFrame::new(12),
                clip_index: 0
            })
        );
        assert_eq!(resolve_single(GlobalFrame::new(0), 0), None);
        assert_eq!(
            resolve_single(GlobalFrame::new(99), 50),
            Some(BackendAim {
                global: GlobalFrame::new(49),
                frame: ClipFrame::new(49),
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
        let at_cut = resolve_playlist(&model, GlobalFrame::new(10)).expect("second clip start");
        assert_eq!(at_cut.clip_index, 1);
        assert_eq!(at_cut.frame, ClipFrame::new(0));
        assert_eq!(at_cut.global, GlobalFrame::new(10));
        let last_first = resolve_playlist(&model, GlobalFrame::new(9)).expect("end of first");
        assert_eq!(last_first.clip_index, 0);
        assert_eq!(last_first.frame, ClipFrame::new(9));
    }
}
