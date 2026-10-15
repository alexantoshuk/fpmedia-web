//! Audio demux + decode/schedule (isolated from video / canvas).
//!
//! - Always: [`demux`] (`Mp4AudioTrack`) + [`stretch`] (WSOLA pitch-preserving rate)
//!   + [`pcm`] (PCM packet → f32; no WebCodecs decoder).
//! - `wasm32` + `webcodecs`: [`output`] (`AudioDecoder` / PCM → `AudioContext`) +
//!   [`owned_audio_data`] + [`playback_clock`] (no-audio timeline: wall time + muted
//!   `AudioContext` for resume — mediabunny media-player `getPlaybackTime`).
//!
//! [`crate::clock::MediaClock`] stays shared (wall or audio-driven timeline).
//! UI hosts (`fpmedia_web_components`) only call engine APIs —
//! no keyboard host UI here.

mod demux;
pub mod pcm;
mod pcm_entry;
mod stretch;

#[cfg(all(target_arch = "wasm32", feature = "webcodecs"))]
mod output;
#[cfg(all(target_arch = "wasm32", feature = "webcodecs"))]
mod owned_audio_data;
#[cfg(all(target_arch = "wasm32", feature = "webcodecs"))]
mod playback_clock;

pub(crate) use demux::build_audio_track;
pub use demux::{AudioSampleEntry, AudioSampleId, Mp4AudioTrack};
pub use pcm::PcmFormat;
pub use stretch::TimeStretcher;

#[cfg(all(target_arch = "wasm32", feature = "webcodecs"))]
pub use output::AudioOutput;
#[cfg(all(target_arch = "wasm32", feature = "webcodecs"))]
pub use owned_audio_data::OwnedAudioData;
#[cfg(all(target_arch = "wasm32", feature = "webcodecs"))]
pub(crate) use playback_clock::PlaybackClock;
