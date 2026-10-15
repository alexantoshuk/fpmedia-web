# fpmedia-timecode

SMPTE timecode math: absolute frames ↔ `HH:MM:SS:FF` (non-drop) / `HH:MM:SS;FF`
(drop-frame). Leaf crate — no media I/O.

```rust
use fpmedia_timecode::{FrameRate, Timecode};

let tc = Timecode::parse("01:00:00:00", FrameRate::Fps24).unwrap();
assert_eq!(tc.total_frames(), 86_400);
assert_eq!(
    Timecode::from_frames(24, FrameRate::Fps24).to_string(),
    "00:00:01:00"
);
```

Drop-frame uses the Heidelberger formulation of SMPTE ST 12-1 (same family of
math as eoyilmaz/timecode). Embedded container start TC (`tmcd`) is handled in
`fpmedia-web` demux, not here.

License: MIT OR Apache-2.0 (see repo `LICENSE`).
