//! Decoder-fault helpers for [`super::VideoPlayer`]. Timers: [`crate::timer`].

use crate::error::MediaError;

/// WebCodecs poison after densify/settle abort races (Exact retry once).
///
/// **Named recovery predicate** (not a [`crate::SettlePolicy`] knob) — classifies
/// closed-codec / Decoding error / wedged backpressure so Exact/PlayPrime can
/// soft-cold or recreate once. Includes wedged `decodeQueueSize backpressure
/// timed out` after the reset+retry in [`super::VideoPlayer`] decode (`q≈41,held=0`).
pub(super) fn is_decoder_poison(err: &MediaError) -> bool {
    let MediaError::WebCodecs(msg) = err else {
        return false;
    };
    let m = msg.to_ascii_lowercase();
    m.contains("decoding error")
        || m.contains("closed")
        || m.contains("cannot call")
        || m.contains("key frame required")
        || m.contains("decodequeuesize backpressure timed out")
}
