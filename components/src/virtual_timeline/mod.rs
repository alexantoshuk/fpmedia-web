//! Virtual Timeline — shared scrub settle / resolve (+ thumbs progressive; later play · LoopZone).
//!
//! **Policy lives here** (global FrameId axis). Exact bitmaps = **slot** blit only.
//! Covered + Fetch + thumb **markers** = **sole on VT** ([`ExactLedger`]). Densify gap-pick
//! still uses slot `ScrubChunkStore`; host mirrors → `note_covered` / `note_fetch_run` /
//! `note_thumb`. Thumb bitmaps stay in slot ThumbCache (≤2 sinks).
//! UI binding: [`crate::TimelineRef`].
//! Design: `.ai/architecture.md` § Virtual Timeline.
//! **no equivalent** (scrub / playlist composition local — not a mediabunny port).

mod densify_plan;
mod ledger;
mod orchestrate;
mod resolve;
mod scrub;
mod thumb_plan;

#[cfg(target_arch = "wasm32")]
mod backend;

pub use densify_plan::{DensifyPlanInput, plan_next_densify_gap};
pub use ledger::{ExactLedger, PutExactOutcome};
pub use orchestrate::{
    plan_timeline_densify_gap, resolve_densify_playlist, resolve_densify_single,
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
