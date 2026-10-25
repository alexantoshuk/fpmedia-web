//! SMPTE timecode math for frame-precise review.
//!
//! Bidirectional conversion between absolute frame counts and `HH:MM:SS:FF`
//! (non-drop) / `HH:MM:SS;FF` (drop-frame). Drop-frame follows the Heidelberger
//! formulation of SMPTE ST 12-1 (same math as eoyilmaz/timecode).
//!
//! [`Timecode`] always carries its [`FrameRate`] — pass rate only at construction.
//!
//! Embedded container start TC (`tmcd` / XMP) is out of scope; use
//! [`Timecode::from_start`] / [`Timecode::to_frame_from_start`] once you have a start value.

mod dropframe;
mod error;
mod format;
mod framerate;
mod parse;
mod timecode;

pub use error::TimecodeError;
pub use framerate::{FrameRate, Rational};
pub use timecode::Timecode;
