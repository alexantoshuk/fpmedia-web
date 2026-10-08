//! Pure engine state machines (`reduce(state, event) -> (state, cmds)`).
//!
//! Design: `.ai/architecture.md` § State machines over flags.

pub mod clock_state;
pub mod decoder_slot;
pub mod engine_mode;
pub mod session;
pub mod transport;
