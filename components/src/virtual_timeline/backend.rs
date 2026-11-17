//! Wasm adapters for [`super::ExactLedger`] ↔ slot Covered markers.
//!
//! **Covered sole on VT:** densify stays on slot `ScrubChunkStore` for gap-pick;
//! host mirrors local Covered → global [`ExactLedger::note_covered`] for the strip.
//! Exact bitmaps stay in slot blit — do not revive Exact ledger write / take.
//! **no equivalent** (composition local).

use std::cell::RefCell;
use std::rc::Rc;

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
    covered_local_ids: impl IntoIterator<Item = u64>,
    local_to_global: impl Fn(u64) -> Option<u64>,
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
    local_to_global: impl Fn(u32) -> Option<u64>,
) -> bool {
    let ids = video.scrub_covered_frame_ids();
    note_covered_from_locals(ledger, ids.into_iter().map(u64::from), |local| {
        u32::try_from(local).ok().and_then(&local_to_global)
    })
}
