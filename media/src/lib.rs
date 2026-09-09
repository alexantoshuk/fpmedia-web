//! Frame-accurate media engine for webmedia.
//!
//! Isolated from the Leptos/`app` stack.
//!
//! - Always: MP4 demux + presentation-order frame index + player API (encoded samples).
//! - Feature `webcodecs` on `wasm32`: WebCodecs decode → canvas via [`WebPlayer`].
//!
//! Frame index (presentation order) is the primary position identifier —
//! timestamps are derived from the sample table, not `time * fps`.

mod demux;
mod error;
mod frame_index;
mod player;
mod types;

#[cfg(all(target_arch = "wasm32", feature = "webcodecs"))]
mod decode;

pub use demux::{Mp4VideoTrack, open_mp4_bytes};
#[cfg(not(target_arch = "wasm32"))]
pub use demux::open_mp4_path;
pub use error::MediaError;
pub use frame_index::{FrameEntry, FrameIndex};
pub use player::{DecodeWindow, Player, PlayerState};
pub use types::{
    CodecConfig, EncodedSample, FrameId, Timestamp, VideoCodec, VideoMetadata,
};

#[cfg(all(target_arch = "wasm32", feature = "webcodecs"))]
pub use decode::{WebCodecsDecoder, WebPlayer, draw_frame_to_canvas};
