//! Frame axes of the playlist / virtual timeline (composition local).
//!
//! Three axes meet at a playlist clip:
//!
//! | Axis | Type | Meaning |
//! | ---- | ---- | ------- |
//! | Source | [`FrameId`] | presentation frame of the open file (engine, `VideoRef`) |
//! | Clip | [`ClipFrame`] | position inside the clip as the playlist sees it |
//! | Global | [`GlobalFrame`] | position on the virtual timeline (Σ clip frames) |
//!
//! Without a cut, Clip == Source. [`ClipFrame::source`] / [`ClipFrame::from_source`]
//! are the **only** places a future CutWindow `cut_in` offset belongs.
//! Vocab: `.ai/glossary.md` § ClipFrame / GlobalFrame. **no equivalent** (composition local).

use core::fmt;

use fpmedia_web::FrameId;

/// Position inside a playlist clip (see the module table).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ClipFrame(FrameId);

impl ClipFrame {
    pub const fn new(frame: FrameId) -> Self {
        Self(frame)
    }

    /// Raw index for readouts and clip-local arithmetic.
    pub const fn get(self) -> FrameId {
        self.0
    }

    /// Clip → Source seam: the frame to hand to the engine / `VideoRef`.
    pub const fn source(self) -> FrameId {
        self.0
    }

    /// Source → Clip seam: a frame reported by the engine / `VideoRef`.
    pub const fn from_source(frame: FrameId) -> Self {
        Self(frame)
    }
}

impl fmt::Display for ClipFrame {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// Position on the virtual timeline (Σ clip frames; identity for a single clip).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct GlobalFrame(FrameId);

impl GlobalFrame {
    pub const fn new(frame: FrameId) -> Self {
        Self(frame)
    }

    pub const fn get(self) -> FrameId {
        self.0
    }
}

impl fmt::Display for GlobalFrame {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}
