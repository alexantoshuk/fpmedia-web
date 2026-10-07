//! Browser video player façade: demux + WebCodecs decode + canvas (+ optional audio).
//!
//! Decode plumbing lives in [`crate::decode`]; this module owns play/scrub/seek orchestration.
//! Present and decode share the player via interior mutability so an rAF present loop
//! never waits on `prefetch_one` (mediabunny-style split).
//!
//! Layout (navigation splits P0–P6): `api` · `shared` · `util` · `open` · `control` ·
//! `clock_ctrl` · `present` · `decode_ops` · `play` · `scrub*` · `seek`.

use std::cell::RefCell;
use std::rc::Rc;

use crate::decode::webcodecs::WebCodecsDecoder;

mod api;
mod clock_ctrl;
mod control;
mod decode_ops;
mod open;
mod play;
mod present;
mod scrub;
mod scrub_cold;
mod scrub_gap;
mod seek;
mod shared;
mod util;

pub use api::{DuePresent, PipelineStats, ReadyFrameKind, ReadyFrameMark};
pub(crate) use shared::PRESENTATION_QUEUE_CAP;
// Private imports — child mods (`play` / `scrub` / `seek`) see these via `super::`.
use api::ready_kind_paint_order;
use shared::{
    BIG_LAG_SECONDS, LOOP_NEAR_END_PRESENTATION_CAP_MAX, PLAY_EXACT_VRAM_VF_SLACK,
    PREFETCH_DEADLINE_SLACK_MS, QueuedFrame, SHUTTLE_PRESENTATION_QUEUE_CAP, Shared,
};
use util::{
    is_decoder_poison, performance_now_ms, sleep_ms, wait_animation_frame, yield_event_loop,
};

/// Frame-accurate video player (WebCodecs → canvas); optional audio via [`crate::AudioOutput`].
/// Pure-audio hosts will use a separate `AudioPlayer` over the same audio modules.
pub struct VideoPlayer {
    shared: Rc<Shared>,
    /// `None` only while an async decode owns the decoder (must not re-enter).
    decoder: RefCell<Option<WebCodecsDecoder>>,
}
