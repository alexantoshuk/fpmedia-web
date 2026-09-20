//! Audio demux + decode/schedule (isolated from video / canvas).
//!
//! - Always: [`demux`] (`Mp4AudioTrack`) + [`stretch`] (WSOLA pitch-preserving rate).
//! - `wasm32` + `webcodecs`: [`output`] (`AudioDecoder` → `AudioContext`) + [`owned_audio_data`].
//!
//! [`crate::clock::MediaClock`] stays shared (wall or audio-driven timeline).
//! UI hosts (`media_components`) only call engine APIs —
//! no keyboard chrome here.

mod demux;
mod stretch;

#[cfg(all(target_arch = "wasm32", feature = "webcodecs"))]
mod output;
#[cfg(all(target_arch = "wasm32", feature = "webcodecs"))]
mod owned_audio_data;

pub(crate) use demux::build_audio_track;
pub use demux::{AudioSampleEntry, AudioSampleId, Mp4AudioTrack};
pub use stretch::TimeStretcher;

#[cfg(all(target_arch = "wasm32", feature = "webcodecs"))]
pub use output::AudioOutput;
#[cfg(all(target_arch = "wasm32", feature = "webcodecs"))]
pub use owned_audio_data::OwnedAudioData;
