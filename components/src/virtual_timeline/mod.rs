//! Virtual Timeline — shared scrub / Exact / (later play · LoopZone) policy.
//!
//! **Policy lives here** (global FrameId axis). Backend adapters map Single
//! [`crate::VideoRef`] / Playlist slots onto decode/open/present only.
//! UI binding remains [`crate::TimelineRef`].
//!
//! Residency ledger: Exact AimWindow + Covered on timeline FrameIds.
//! Densify plan + host kick; slots stay decode backends (take → ledger).
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
    LedgerHandle, TimelineExactLedger, import_exact, import_exact_from_player, new_ledger_handle,
    plan_ledger_settle, present_ledger_exact, push_take_from_player, push_take_from_video_ref,
    put_ledger_exact, retarget_ledger_aim, sync_vt_residents_to_player,
    sync_vt_residents_to_video_ref,
};
