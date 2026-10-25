//! Browser video player façade: demux + WebCodecs decode + canvas (+ optional audio).
//!
//! Decode plumbing lives in [`crate::decode`]; this module owns play/scrub/seek orchestration.
//! Present and decode share the player via interior mutability so an rAF render
//! never waits on the Play iterator (mediabunny `render` / `updateNextFrame` split).
//!
//! Layout (navigation splits P0–P6): `api` · `shared` · `open` · `control` ·
//! `clock_ctrl` · `present` · `decode_ops` · `play` · `play_pool` · `scrub*` ·
//! `seek` · `thumb`.

use std::cell::RefCell;
use std::rc::Rc;

use crate::decode::webcodecs::WebCodecsDecoder;

mod api;
mod clock_ctrl;
mod control;
mod decode_ops;
mod open;
mod play;
mod play_pool;
mod present;
mod scrub;
mod scrub_cold;
mod scrub_gap;
mod seek;
mod shared;
#[cfg_attr(not(feature = "thumbs"), path = "thumb_off.rs")]
mod thumb;

pub use api::{
    DuePresent, PipelineInterest, PipelineStats, PlayTick, PlayWaits, PlayerState, ReadyFrameKind,
    ReadyFrameMark,
};
pub(crate) use shared::PRESENTATION_QUEUE_CAP;
// Private imports — child mods (`play` / `scrub` / `seek`) see these via `super::`.
use crate::timer::{next_animation_frame, now_ms, sleep_ms, yield_microtask};
use api::ready_kind_paint_order;
use shared::{PLAY_EXACT_VRAM_VF_SLACK, Shared};

/// Frame-precise video player (WebCodecs → canvas); optional audio via [`crate::AudioOutput`].
/// Pure-audio hosts will use a separate `AudioPlayer` over the same audio modules.
pub struct VideoPlayer {
    shared: Rc<Shared>,
    /// `None` only while an async decode owns the decoder (must not re-enter).
    decoder: RefCell<Option<WebCodecsDecoder>>,
}
