//! Container demuxers.

mod mp4;

pub use mp4::{Mp4VideoTrack, open_mp4_bytes};
#[cfg(not(target_arch = "wasm32"))]
pub use mp4::open_mp4_path;
