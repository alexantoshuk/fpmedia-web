//! Wasm adapters for [`super::ExactLedger`] ↔ slot Covered / Fetch markers.
//!
//! **Covered + Fetch sole on VT:** densify stays on slot `ScrubChunkStore` for gap-pick;
//! host mirrors local Covered → global [`ExactLedger::note_covered`] and playable Fetch
//! → [`ExactLedger::note_fetch_run`] for the strip. Exact bitmaps stay in slot blit —
//! do not revive Exact ledger write / take.
//! **no equivalent** (composition local).

use std::cell::RefCell;
use std::rc::Rc;

use fpmedia_web::FrameId;
use web_sys::ImageBitmap;

use super::ExactLedger;

pub type TimelineExactLedger = ExactLedger<ImageBitmap>;

pub type LedgerHandle = Rc<RefCell<TimelineExactLedger>>;

pub fn new_ledger_handle() -> LedgerHandle {
    Rc::new(RefCell::new(ExactLedger::new()))
}

/// Note slot Covered locals into the ledger (global FrameIds). Returns if any new.
pub fn note_covered_from_locals(
    ledger: &mut TimelineExactLedger,
    covered_local_ids: impl IntoIterator<Item = FrameId>,
    local_to_global: impl Fn(FrameId) -> Option<FrameId>,
) -> bool {
    let mut changed = false;
    for local in covered_local_ids {
        let Some(global) = local_to_global(local) else {
            continue;
        };
        if ledger.has_covered(global) {
            continue;
        }
        ledger.note_covered(global);
        changed = true;
    }
    changed
}

/// Same as [`note_covered_from_locals`] via [`crate::VideoRef`] (Single / Playlist slot).
pub fn note_covered_from_video_ref(
    ledger: &mut TimelineExactLedger,
    video: &crate::VideoRef,
    local_to_global: impl Fn(FrameId) -> Option<FrameId>,
) -> bool {
    let ids = video.scrub_covered_frame_ids();
    note_covered_from_locals(ledger, ids, local_to_global)
}

/// Note slot playable Fetch runs into the ledger (global FrameIds). Returns if union grew.
pub fn note_fetch_from_locals(
    ledger: &mut TimelineExactLedger,
    local_runs: impl IntoIterator<Item = (FrameId, FrameId)>,
    local_to_global: impl Fn(FrameId) -> Option<FrameId>,
) -> bool {
    let mut changed = false;
    for (lo, hi) in local_runs {
        let Some(glo) = local_to_global(lo) else {
            continue;
        };
        let Some(ghi) = local_to_global(hi) else {
            continue;
        };
        changed |= ledger.note_fetch_run(glo, ghi);
    }
    changed
}

/// Same as [`note_fetch_from_locals`] via [`crate::VideoRef`] (Single / Playlist slot).
pub fn note_fetch_from_video_ref(
    ledger: &mut TimelineExactLedger,
    video: &crate::VideoRef,
    local_to_global: impl Fn(FrameId) -> Option<FrameId>,
) -> bool {
    let runs = video.scrub_fetch_playable_runs();
    note_fetch_from_locals(ledger, runs, local_to_global)
}
