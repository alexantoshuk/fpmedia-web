//! Wasm adapters: VT Exact ledger ↔ slot `VideoPlayer` / `VideoRef`.
//!
//! Ledger owns Exact/Covered residency. Slot blit = decode scratch → **take**
//! into ledger (no clone-mirror sync). **no equivalent** (composition local).

use std::cell::RefCell;
use std::rc::Rc;

use fpmedia_web::VideoPlayer;
use web_sys::ImageBitmap;

use super::{ExactLedger, ExactSettlePlan, PutExactOutcome, plan_exact_settle};

pub type TimelineExactLedger = ExactLedger<ImageBitmap>;

pub type LedgerHandle = Rc<RefCell<TimelineExactLedger>>;

pub fn new_ledger_handle() -> LedgerHandle {
    Rc::new(RefCell::new(ExactLedger::new()))
}

fn release_bitmaps(out: PutExactOutcome<ImageBitmap>) {
    if let Some(b) = out.replaced {
        b.close();
    }
    for (_, b) in out.evicted {
        b.close();
    }
    if let Some(b) = out.refused {
        b.close();
    }
}

/// Put Exact under `global` without moving AimWindow aim (bulk densify import).
pub fn put_ledger_exact(ledger: &mut TimelineExactLedger, global: u64, bitmap: ImageBitmap) {
    release_bitmaps(ledger.put_exact(global, bitmap));
}

/// Settle / single-frame import: retarget aim to `global`, orphan-evict, put.
pub fn import_exact(ledger: &mut TimelineExactLedger, global: u64, bitmap: ImageBitmap) {
    ledger.set_aim(global);
    for (_, bmp) in ledger.take_orphans_beyond_aim() {
        bmp.close();
    }
    put_ledger_exact(ledger, global, bitmap);
}

/// Take Exact from the player blit (if any) → ledger at `global`.
pub fn import_exact_from_player(
    ledger: &mut TimelineExactLedger,
    player: &VideoPlayer,
    local: u64,
    global: u64,
) {
    let Some(bmp) = player.take_scrub_exact_blit(local) else {
        return;
    };
    import_exact(ledger, global, bmp);
}

/// Present ledger Exact onto the player canvas (`local` FrameId for commit).
pub fn present_ledger_exact(
    ledger: &TimelineExactLedger,
    player: &VideoPlayer,
    global: u64,
    local: u64,
) -> bool {
    let Some(bmp) = ledger.get_exact(global) else {
        return false;
    };
    player.present_image_bitmap(local, bmp).is_ok()
}

/// Pause Exact settle via ledger: PresentHit or caller must decode then import.
pub fn plan_ledger_settle(ledger: &TimelineExactLedger, global: u64) -> ExactSettlePlan {
    plan_exact_settle(ledger.has_exact(global))
}

/// Retarget aim + drop Exact farther than capacity radius. Returns if strip changed.
pub fn retarget_ledger_aim(ledger: &mut TimelineExactLedger, aim: u64) -> bool {
    ledger.set_aim(aim);
    let victims = ledger.take_orphans_beyond_aim();
    let changed = !victims.is_empty();
    for (_, bmp) in victims {
        bmp.close();
    }
    changed
}

/// Push local Exact ids currently in the ledger onto the slot (densify skip set).
///
/// Call after [`push_take_from_player`] / aim orphan so fill does not re-decode
/// Exact already taken into the ledger. **no equivalent**.
pub fn sync_vt_residents_to_player(
    ledger: &TimelineExactLedger,
    player: &VideoPlayer,
    global_to_local: impl Fn(u64) -> Option<u64>,
) {
    let locals: Vec<u64> = ledger
        .exact_frame_ids()
        .into_iter()
        .filter_map(global_to_local)
        .collect();
    player.set_vt_exact_residents(&locals);
}

/// Same as [`sync_vt_residents_to_player`] via [`crate::VideoRef`].
pub fn sync_vt_residents_to_video_ref(
    ledger: &TimelineExactLedger,
    video: &crate::VideoRef,
    global_to_local: impl Fn(u64) -> Option<u32>,
) {
    let locals: Vec<u32> = ledger
        .exact_frame_ids()
        .into_iter()
        .filter_map(global_to_local)
        .collect();
    video.set_vt_exact_residents(&locals);
}

/// Move slot Exact (+ note Covered) into the ledger — sole residency write path.
///
/// `timeline_aim`: when `Some`, retarget + orphan-evict once. Puts never per-id
/// `set_aim`. **Take** from slot (not clone) so VRAM is not dual-resident.
/// Engine skip uses [`sync_vt_residents_to_player`] after take. **no equivalent**.
pub fn push_take_from_player(
    ledger: &mut TimelineExactLedger,
    player: &VideoPlayer,
    timeline_aim: Option<u64>,
    local_to_global: impl Fn(u64) -> Option<u64>,
) -> bool {
    push_take_residency(
        ledger,
        player.scrub_exact_capacity(),
        timeline_aim,
        player.scrub_exact_frame_ids(),
        player.scrub_covered_frame_ids(),
        |local| player.take_scrub_exact_blit(local),
        local_to_global,
    )
}

/// Same as [`push_take_from_player`] via host [`crate::VideoRef`] (Playlist).
pub fn push_take_from_video_ref(
    ledger: &mut TimelineExactLedger,
    video: &crate::VideoRef,
    timeline_aim: Option<u64>,
    local_to_global: impl Fn(u32) -> Option<u64>,
) -> bool {
    let exact_ids: Vec<u64> = video
        .scrub_exact_frame_ids()
        .into_iter()
        .map(u64::from)
        .collect();
    let covered_ids: Vec<u64> = video
        .scrub_covered_frame_ids()
        .into_iter()
        .map(u64::from)
        .collect();
    push_take_residency(
        ledger,
        video.scrub_exact_capacity(),
        timeline_aim,
        exact_ids,
        covered_ids,
        |local| {
            u32::try_from(local)
                .ok()
                .and_then(|l| video.take_scrub_exact_blit(l))
        },
        |local| u32::try_from(local).ok().and_then(&local_to_global),
    )
}

fn push_take_residency(
    ledger: &mut TimelineExactLedger,
    cap: usize,
    timeline_aim: Option<u64>,
    exact_local_ids: Vec<u64>,
    covered_local_ids: Vec<u64>,
    mut take_exact: impl FnMut(u64) -> Option<ImageBitmap>,
    local_to_global: impl Fn(u64) -> Option<u64>,
) -> bool {
    let mut changed = false;
    if ledger.capacity() != cap {
        for (_, bmp) in ledger.set_capacity(cap) {
            bmp.close();
        }
        changed = true;
    }
    if let Some(aim) = timeline_aim {
        changed |= retarget_ledger_aim(ledger, aim);
    }
    for local in exact_local_ids {
        let Some(global) = local_to_global(local) else {
            continue;
        };
        let Some(bmp) = take_exact(local) else {
            continue;
        };
        if ledger.has_exact(global) {
            bmp.close();
            continue;
        }
        put_ledger_exact(ledger, global, bmp);
        changed = true;
    }
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
