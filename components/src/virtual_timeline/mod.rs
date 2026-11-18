//! Virtual Timeline — shared scrub settle / resolve (+ later play · LoopZone).
//!
//! **Policy lives here** (global FrameId axis). Exact bitmaps = **slot** blit only.
//! Covered + Fetch markers = **sole on VT** ([`ExactLedger`] strip). Densify gap-pick
//! still uses slot `ScrubChunkStore`; host mirrors → `note_covered` / `note_fetch_run`.
//! UI binding: [`crate::TimelineRef`].
//! Design: `.ai/architecture.md` § Virtual Timeline.
//! **no equivalent** (scrub / playlist composition local — not a mediabunny port).

mod densify_plan;
mod ledger;
mod orchestrate;
mod resolve;
mod scrub;

#[cfg(target_arch = "wasm32")]
mod backend;

pub use densify_plan::{DensifyPlanInput, plan_next_densify_gap};
pub use ledger::{ExactLedger, PutExactOutcome};
pub use orchestrate::{
    plan_timeline_densify_gap, resolve_densify_playlist, resolve_densify_single,
};
pub use resolve::{BackendAim, resolve_playlist, resolve_single};
pub use scrub::{ExactSettlePlan, plan_exact_settle};

#[cfg(target_arch = "wasm32")]
pub use backend::{
    LedgerHandle, TimelineExactLedger, new_ledger_handle, note_covered_from_locals,
    note_covered_from_video_ref, note_fetch_from_locals, note_fetch_from_video_ref,
};
