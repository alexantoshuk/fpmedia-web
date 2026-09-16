//! Browser decode / render (WebCodecs + canvas).

mod audio_output;
mod canvas;
mod frame_cache;
mod owned_audio_data;
mod owned_video_frame;
mod play_encode_worker;
mod scrub_encode;
mod web_player;
mod webcodecs;

pub use audio_output::AudioOutput;
pub use canvas::{
    canvas_2d, configure_canvas_for_video, draw_frame_to_canvas, draw_frame_with_context,
};
pub use owned_audio_data::OwnedAudioData;
pub use owned_video_frame::{AsVideoFrame, OwnedVideoFrame};
pub use web_player::{
    DuePresent, PRESENTATION_QUEUE_CAP, STEP_BEHIND_CAP, PipelineStats, ReadyFrameKind,
    ReadyFrameMark, WebPlayer,
};
pub use webcodecs::WebCodecsDecoder;
