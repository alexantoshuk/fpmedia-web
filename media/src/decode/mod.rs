//! Browser decode / render (WebCodecs + canvas).

mod canvas;
mod frame_cache;
mod owned_video_frame;
mod webcodecs;
mod web_player;

pub use canvas::{
    canvas_2d, configure_canvas_for_video, draw_frame_to_canvas, draw_frame_with_context,
};
pub use owned_video_frame::{AsVideoFrame, OwnedVideoFrame};
pub use web_player::{
    DuePresent, PipelineStats, PRESENTATION_QUEUE_CAP, ReadyFrameKind, ReadyFrameMark, WebPlayer,
};
pub use webcodecs::WebCodecsDecoder;
