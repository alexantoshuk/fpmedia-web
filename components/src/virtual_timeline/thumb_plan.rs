//! VT Pause progressive thumb next-key (global FrameId axis).
//!
//! Bitmaps = VT [`super::VtThumbCache`]; this plans *which* global sync key to
//! fill next across Single (identity) or Playlist (Σ).
//! **no equivalent** (filmstrip / scrub local). Decode: `mediabunny:` CanvasSink.

use fpmedia_web::{FrameId, next_progressive_sync_key, thumb_progressive_coarse_count};

use super::BackendAim;
use crate::video_playlist::PlaylistModel;

/// Inputs for [`plan_next_thumb_fill`].
#[derive(Clone, Debug)]
pub struct ThumbFillPlanInput<'a> {
    /// Sorted unique sync keys in **global** FrameIds.
    pub global_sync_keys: &'a [FrameId],
    /// Presentation duration of the whole timeline (playlist = Σ seconds).
    pub duration_secs: f64,
    /// VT / sink thumb cap (`thumb_cache_frame_budget` / `vt_thumb_max_default`).
    pub max_thumbs: usize,
}

/// Next global sync key for Pause progressive, or `None` when coarse→densify done.
pub fn plan_next_thumb_fill(
    input: &ThumbFillPlanInput<'_>,
    ready: impl Fn(FrameId) -> bool,
) -> Option<FrameId> {
    let coarse = thumb_progressive_coarse_count(
        input.duration_secs,
        input.global_sync_keys.len(),
        input.max_thumbs,
    );
    next_progressive_sync_key(input.global_sync_keys, ready, coarse)
}

/// Map local sync keys for one clip onto the playlist Σ axis.
pub fn map_local_sync_keys_to_global(
    model: &PlaylistModel,
    clip_index: u32,
    local_keys: &[FrameId],
) -> Vec<FrameId> {
    let Some(start) = model.clip_start_frame(clip_index) else {
        return Vec::new();
    };
    let start = FrameId::from(start);
    local_keys
        .iter()
        .copied()
        .map(|local| start.saturating_add(local))
        .collect()
}

/// Resolve a planned global thumb key onto a decode backend.
pub fn resolve_thumb_fill_playlist(model: &PlaylistModel, global: FrameId) -> Option<BackendAim> {
    super::resolve_playlist(model, global)
}

/// Single-clip: global == local.
pub fn resolve_thumb_fill_single(global: FrameId, frame_count: FrameId) -> Option<BackendAim> {
    super::resolve_single(global, frame_count)
}

/// Merge per-clip local sync key lists into one sorted unique global list.
pub fn merge_global_sync_keys(
    model: &PlaylistModel,
    per_clip: &[(u32, Vec<FrameId>)],
) -> Vec<FrameId> {
    let mut out = Vec::new();
    for &(clip, ref locals) in per_clip {
        out.extend(map_local_sync_keys_to_global(model, clip, locals));
    }
    out.sort_unstable();
    out.dedup();
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::video_playlist::PlaylistClip;

    fn two_clip_model() -> PlaylistModel {
        PlaylistModel::new(vec![
            PlaylistClip {
                frame_count: 100,
                duration_secs: 10.0,
                fps: Some(10.0),
            },
            PlaylistClip {
                frame_count: 100,
                duration_secs: 10.0,
                fps: Some(10.0),
            },
        ])
    }

    #[test]
    fn merge_maps_second_clip_offset() {
        let model = two_clip_model();
        let keys = merge_global_sync_keys(&model, &[(0, vec![0, 50]), (1, vec![0, 50])]);
        assert_eq!(keys, vec![0, 50, 100, 150]);
    }

    #[test]
    fn plan_prefers_coarse_across_splice() {
        let model = two_clip_model();
        let keys = merge_global_sync_keys(
            &model,
            &[(0, vec![0, 25, 50, 75]), (1, vec![0, 25, 50, 75])],
        );
        // Σ duration 20s → coarse ~7 (clamp 8 min) but key_count/budget gates.
        let input = ThumbFillPlanInput {
            global_sync_keys: &keys,
            duration_secs: model.total_duration_secs(),
            max_thumbs: 256,
        };
        let mut ready = std::collections::BTreeSet::<FrameId>::new();
        let first = plan_next_thumb_fill(&input, |g| ready.contains(&g)).expect("first");
        assert_eq!(first, 0);
        ready.insert(first);
        let second = plan_next_thumb_fill(&input, |g| ready.contains(&g)).expect("second");
        // Coarse spreads across full key list — not only clip 0 densify.
        assert!(second > 0);
        let aim = resolve_thumb_fill_playlist(&model, second).expect("resolve");
        assert!(aim.clip_index <= 1);
    }

    #[test]
    fn single_identity_resolve() {
        let aim = resolve_thumb_fill_single(12, 100).expect("ok");
        assert_eq!(aim.global, 12);
        assert_eq!(aim.local, 12);
        assert_eq!(aim.clip_index, 0);
    }
}
