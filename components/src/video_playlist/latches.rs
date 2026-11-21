//! Playlist host coalesce latches + VT thumb fill state (plain data; no Leptos).
//!
//! One `StoredValue` each on [`super::video_playlist_ref::VideoPlaylistRef`] —
//! fields that are cleared together live together. No FSM: each field is an
//! independent "already kicked / in flight" marker. **no equivalent** (host local).

use std::collections::{HashMap, HashSet};

use fpmedia_web::FrameId;

/// "Already kicked" markers that stop Effect / Move stampedes from re-spawning work.
///
/// `Copy`: read with `get_value()`, write with `update_value`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
pub(crate) struct HostLatches {
    /// Clip index already kicked for near-EOF origin warm this lap (`None` = none).
    /// Cleared on seam swap / LoopZone wrap / close — **not** per open gen.
    pub(crate) seam_warm_for: Option<u32>,
    /// In-flight `ensure_clip_on_idle` target — coalesce Move stampede.
    pub(crate) ensure_idle_for: Option<u32>,
    /// In-flight cut-join Exact warm `(idle_clip, join_local)` — one-shot coalesce.
    pub(crate) cut_join_warm_for: Option<(u32, u32)>,
    /// Cross-clip scrub: outgoing hard-paused once per gesture (not every Move).
    pub(crate) scrub_cross_paused: bool,
}

#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
impl HostLatches {
    /// New open generation: drop the per-gen coalesce latches.
    ///
    /// Leaves [`Self::seam_warm_for`] alone — it is a per-lap latch owned by the
    /// seam / loop code (and `close`), not by the open generation.
    pub(crate) fn reset_for_new_gen(&mut self) {
        self.ensure_idle_for = None;
        self.cut_join_warm_for = None;
        self.scrub_cross_paused = false;
    }
}

/// VT progressive thumb fill bookkeeping (Pause only).
///
/// Not `Copy` — read one field via `with_value`, write via `update_value`
/// (`get_value()` would clone every collection on the Effect hot path).
#[derive(Debug, Default)]
#[cfg_attr(
    any(not(target_arch = "wasm32"), not(feature = "thumbs")),
    allow(dead_code)
)]
pub(crate) struct ThumbFillState {
    /// Per-clip sync FrameIds (local) for VT thumb progressive — survives idle rebind.
    pub(crate) sync_keys: HashMap<u32, Vec<FrameId>>,
    /// One VT progressive / hover fill in flight (coalesce Effect kicks).
    pub(crate) inflight: bool,
    /// Globals skipped after admit/cap refuse (must not spin; not "bitmap ready").
    pub(crate) skip: HashSet<FrameId>,
    /// Clips whose open-slot progressive walk finished this session (idle walk gate).
    pub(crate) clips_done: HashSet<u32>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reset_for_new_gen_keeps_seam_warm_for() {
        let mut l = HostLatches {
            seam_warm_for: Some(2),
            ensure_idle_for: Some(1),
            cut_join_warm_for: Some((1, 40)),
            scrub_cross_paused: true,
        };
        l.reset_for_new_gen();
        assert_eq!(
            l,
            HostLatches {
                seam_warm_for: Some(2),
                ..HostLatches::default()
            }
        );
    }

    #[test]
    fn default_is_all_clear() {
        let l = HostLatches::default();
        assert_eq!(l.seam_warm_for, None);
        assert_eq!(l.ensure_idle_for, None);
        assert_eq!(l.cut_join_warm_for, None);
        assert!(!l.scrub_cross_paused);
        let t = ThumbFillState::default();
        assert!(!t.inflight);
        assert!(t.sync_keys.is_empty() && t.skip.is_empty() && t.clips_done.is_empty());
    }
}
