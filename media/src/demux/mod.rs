//! Container demuxers.

mod boxes;
pub(crate) mod coalesce;
mod mp4;
mod timecode;

pub use boxes::{
    FragmentBackfillPlan, TopLevelBox, advance_mfra_backfill_n, load_ftyp_moov,
    parse_mp4_progressive, parse_mp4_progressive_with_backfill,
    parse_mp4_progressive_with_progress, read_ftyp_moov_stub, scan_top_level,
};
#[cfg(not(target_arch = "wasm32"))]
pub use mp4::open_mp4_path;
pub use mp4::{Mp4VideoTrack, OpenMp4Options, OpenedMp4, open_mp4, open_mp4_bytes};
