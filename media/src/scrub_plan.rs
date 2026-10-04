//! Pure scrub densify / cold-settle planning (native-testable).
//!
//! WASM wrappers in `video_player::scrub_gap` / `scrub_cold` query residency then
//! call these helpers. No WebCodecs / `Shared` here.
//!
//! Local invention (mediabunny: no densify plane); cold path mirrors async retrieve
//! ideas from `mediabunny: src/media-sink.ts` `mediaSamplesAtTimestamps`.

// Callers live under `video_player` (`wasm32` + `webcodecs` only). Native builds
// still compile this module for unit tests.
#![cfg_attr(
    not(all(target_arch = "wasm32", feature = "webcodecs")),
    allow(dead_code)
)]

use crate::types::{FrameId, SettlePolicy};

/// Result of [`decide_next_gap`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NextGapDecision {
    pub target: Option<FrameId>,
    /// Aim left a stalled frame — caller should clear `fill_stalled_aim`.
    pub clear_fill_stalled_aim: bool,
}

/// Pick the next densify gap after blit/marker queries.
///
/// - Clamp to `indexed_last` (fMP4 provisional length may exceed indexed samples).
/// - Stalled at current playhead → no target (do not clear stall).
/// - Blit holes win over marker `no_gaps` (Play trims Exact, keeps markers).
pub fn decide_next_gap(
    playhead: FrameId,
    last: FrameId,
    indexed_last: FrameId,
    fill_stalled_aim: Option<FrameId>,
    blit_gap: Option<FrameId>,
    marker_saturated: bool,
    marker_armed: bool,
    marker_gap: Option<FrameId>,
) -> NextGapDecision {
    let last = last.min(indexed_last);
    let playhead = playhead.min(last);
    if fill_stalled_aim == Some(playhead) {
        return NextGapDecision {
            target: None,
            clear_fill_stalled_aim: false,
        };
    }
    let clear_fill_stalled_aim = fill_stalled_aim.is_some();
    if let Some(gap) = blit_gap {
        return NextGapDecision {
            target: Some(gap),
            clear_fill_stalled_aim,
        };
    }
    if marker_saturated || !marker_armed {
        return NextGapDecision {
            target: None,
            clear_fill_stalled_aim,
        };
    }
    NextGapDecision {
        target: marker_gap,
        clear_fill_stalled_aim,
    }
}

/// Soft-cold settle may only commit within the indexed prefix.
///
/// `None` when nothing is indexed yet (do not `commit_frame`).
pub fn cold_settle_commit_frame(aim: FrameId, indexed_count: FrameId) -> Option<FrameId> {
    if indexed_count == 0 {
        None
    } else {
        Some(aim.min(indexed_count - 1))
    }
}

/// Warm-byte lead for Exact / PlayPrime: clamp GOP length by [`SettlePolicy`].
pub fn gop_sample_lead_frames(gop_len: u64) -> u64 {
    SettlePolicy::default().clamp_gop_sample_lead(gop_len)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stalled_at_playhead_blocks_without_clear() {
        let d = decide_next_gap(10, 100, 100, Some(10), Some(12), false, true, Some(14));
        assert_eq!(
            d,
            NextGapDecision {
                target: None,
                clear_fill_stalled_aim: false
            }
        );
    }

    #[test]
    fn blit_gap_wins_and_clears_stale_stall() {
        let d = decide_next_gap(10, 100, 100, Some(5), Some(12), true, false, None);
        assert_eq!(
            d,
            NextGapDecision {
                target: Some(12),
                clear_fill_stalled_aim: true
            }
        );
    }

    #[test]
    fn clamps_to_indexed_last() {
        // playhead/last provisionally 200, indexed only through 23.
        let d = decide_next_gap(50, 200, 23, None, Some(20), false, true, Some(22));
        // blit_gap is trusted as already queried with clamped bounds by caller;
        // decision still returns blit when present.
        assert_eq!(d.target, Some(20));
    }

    #[test]
    fn marker_disarmed_yields_none() {
        let d = decide_next_gap(10, 100, 100, None, None, false, false, Some(14));
        assert_eq!(d.target, None);
    }

    #[test]
    fn marker_saturated_yields_none() {
        let d = decide_next_gap(10, 100, 100, None, None, true, true, Some(14));
        assert_eq!(d.target, None);
    }

    #[test]
    fn marker_gap_when_armed() {
        let d = decide_next_gap(10, 100, 100, None, None, false, true, Some(14));
        assert_eq!(d.target, Some(14));
    }

    #[test]
    fn cold_commit_indexed_prefix_only() {
        assert_eq!(cold_settle_commit_frame(100, 0), None);
        assert_eq!(cold_settle_commit_frame(100, 24), Some(23));
        assert_eq!(cold_settle_commit_frame(5, 24), Some(5));
    }

    #[test]
    fn gop_lead_clamps_like_settle_policy() {
        assert_eq!(gop_sample_lead_frames(12), 12);
        assert_eq!(gop_sample_lead_frames(4), 8);
        assert_eq!(gop_sample_lead_frames(100), 24);
    }
}
