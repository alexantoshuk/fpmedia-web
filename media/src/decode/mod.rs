//! Browser decode / render (WebCodecs + canvas).

mod canvas;
mod webcodecs;
mod web_player;

pub use canvas::{
    canvas_2d, configure_canvas_for_video, draw_frame_to_canvas, draw_frame_with_context,
};
pub use web_player::{DuePresent, PipelineStats, PRESENTATION_QUEUE_CAP, WebPlayer};
pub use webcodecs::WebCodecsDecoder;
