//! Browser decode / render (WebCodecs + canvas).

mod canvas;
mod webcodecs;
mod web_player;

pub use canvas::draw_frame_to_canvas;
pub use web_player::WebPlayer;
pub use webcodecs::WebCodecsDecoder;
