//! Virtual Timeline — shared scrub settle / resolve (+ thumbs progressive;
//! LoopZone wall / wrap/EOF + SeamPhase FSM; Host wrap exec = playlist `loop_seam_exec`).
//!
//! **Policy lives here** (global FrameId axis). Exact bitmaps = **slot** blit only.
//! Covered + Fetch + thumb **markers** = ledger; thumb **bitmaps** = [`VtThumbCache`]
//! (1 producer — not slot-bound). Densify gap-pick still uses slot `ScrubChunkStore`.
//! UI binding: [`crate::TimelineRef`].
//! Design: `.ai/architecture.md` § Virtual Timeline.
//! **no equivalent** (scrub / playlist composition local — not a mediabunny port).

mod ledger;
mod loop_zone;
mod resolve;
mod scrub;
mod thumb_plan;

#[cfg(target_arch = "wasm32")]
mod backend;
#[cfg(all(target_arch = "wasm32", feature = "thumbs"))]
mod thumb_cache;
#[cfg(all(target_arch = "wasm32", feature = "thumbs"))]
mod thumb_producer;

pub use ledger::{ExactLedger, PutExactOutcome};
pub use loop_zone::{
    AdvanceEndedCtx, AdvanceEndedPlan, HostOutsideCtx, HostOutsidePlan, PlayAtClipLastCtx,
    PlayAtClipLastPlan, ScrubUpCtx, ScrubUpPlan, SeamNextCtx, SeamPhase, StoredZone, WrapDonePlan,
    ZoneOwner, is_soft_seam_cancel, plan_advance_ended, plan_host_outside, plan_play_at_clip_last,
    plan_scrub_up, plan_seam_next_clip, plan_wrap_done, play_outside_zone, resolve_zone,
    seam_ensure_preparing, seam_try_begin, store_zone, zone_owner_from_spans,
};
pub use resolve::{BackendAim, resolve_playlist, resolve_single};
pub use scrub::{ExactSettlePlan, plan_exact_settle};
pub use thumb_plan::{
    ThumbFillPlanInput, map_local_sync_keys_to_global, merge_global_sync_keys,
    plan_next_thumb_fill, resolve_thumb_fill_playlist, resolve_thumb_fill_single,
};

#[cfg(target_arch = "wasm32")]
pub use backend::{
    LedgerHandle, TimelineExactLedger, new_ledger_handle, note_covered_from_locals,
    note_covered_from_video_ref, note_fetch_from_locals, note_fetch_from_video_ref,
};
#[cfg(all(target_arch = "wasm32", feature = "thumbs"))]
pub use thumb_cache::{
    ThumbCacheHandle, VtThumbCache, new_thumb_cache_handle, vt_thumb_max_default,
};
#[cfg(all(target_arch = "wasm32", feature = "thumbs"))]
pub use thumb_producer::{
    ProducerHandle, ThumbProducer, new_producer_handle, rank_for_global_key, try_paint_vt,
};
