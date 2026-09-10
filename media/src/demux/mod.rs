//! Container demuxers.

mod boxes;
mod mp4;

pub use boxes::{
    TopLevelBox, load_ftyp_moov, parse_mp4_progressive, parse_mp4_progressive_with_progress,
    read_ftyp_moov_stub, scan_top_level,
};
pub use mp4::{
    Mp4VideoTrack, OpenMp4Options, open_mp4_bytes, open_mp4_source, open_mp4_source_cached,
    open_mp4_source_cached_with_progress, open_mp4_source_with_progress,
};
#[cfg(not(target_arch = "wasm32"))]
pub use mp4::open_mp4_path;
