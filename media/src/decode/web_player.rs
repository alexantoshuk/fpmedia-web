//! High-level browser player: demux + WebCodecs + canvas.

use web_sys::{HtmlCanvasElement, VideoFrame};

use crate::decode::canvas::draw_frame_to_canvas;
use crate::decode::webcodecs::WebCodecsDecoder;
use crate::error::MediaError;
use crate::player::{Player, PlayerState};
use crate::types::{FrameId, VideoMetadata};

/// Frame-accurate player that decodes with WebCodecs and draws to a canvas.
pub struct WebPlayer {
    player: Player,
    decoder: WebCodecsDecoder,
    canvas: HtmlCanvasElement,
}

impl WebPlayer {
    pub async fn open_mp4_bytes(
        bytes: Vec<u8>,
        canvas: HtmlCanvasElement,
    ) -> Result<Self, MediaError> {
        let player = Player::open_mp4_bytes(bytes)?;
        let decoder = WebCodecsDecoder::create(player.metadata()).await?;
        let mut web = Self {
            player,
            decoder,
            canvas,
        };
        web.seek_frame(0).await?;
        Ok(web)
    }

    pub fn metadata(&self) -> &VideoMetadata {
        self.player.metadata()
    }

    pub fn state(&self) -> PlayerState {
        self.player.state()
    }

    pub fn canvas(&self) -> &HtmlCanvasElement {
        &self.canvas
    }

    pub fn is_playing(&self) -> bool {
        self.state().playing
    }

    pub fn play(&mut self) {
        self.player.play();
    }

    pub fn pause(&mut self) {
        self.player.pause();
    }

    /// Duration of the current frame in seconds (for playback pacing).
    pub fn frame_duration_seconds(&self) -> Result<f64, MediaError> {
        let frame = self.state().frame;
        let entry = self
            .player
            .track()
            .index()
            .get(frame)
            .ok_or(MediaError::FrameOutOfRange(frame, self.state().frame_count))?;
        if entry.duration_ticks == 0 || entry.timescale == 0 {
            return Err(MediaError::MissingFrameDuration(frame));
        }
        Ok(entry.duration_ticks as f64 / entry.timescale as f64)
    }

    pub async fn seek_frame(&mut self, frame: FrameId) -> Result<(), MediaError> {
        self.player.pause();
        self.decoder.invalidate();
        let video_frame = self.decoder.decode_frame(self.player.track(), frame).await?;
        self.present(video_frame)?;
        self.player.commit_frame(frame)?;
        Ok(())
    }

    pub async fn next_frame(&mut self) -> Result<(), MediaError> {
        self.player.pause();
        let next = self.state().frame.saturating_add(1);
        self.goto_frame(next).await
    }

    pub async fn previous_frame(&mut self) -> Result<(), MediaError> {
        self.player.pause();
        let cur = self.state().frame;
        if cur == 0 {
            return Err(MediaError::FrameOutOfRange(0, self.state().frame_count));
        }
        self.decoder.invalidate();
        self.goto_frame(cur - 1).await
    }

    /// Advance one presentation frame while playing.
    ///
    /// Returns `true` if more frames remain, `false` if playback reached the end
    /// (also clears the playing flag).
    pub async fn step_playback(&mut self) -> Result<bool, MediaError> {
        if !self.is_playing() {
            return Ok(false);
        }
        let next = self.state().frame.saturating_add(1);
        if next >= self.state().frame_count {
            self.player.pause();
            return Ok(false);
        }
        self.goto_frame(next).await?;
        Ok(self.state().frame + 1 < self.state().frame_count)
    }

    async fn goto_frame(&mut self, frame: FrameId) -> Result<(), MediaError> {
        let video_frame = self.decoder.decode_frame(self.player.track(), frame).await?;
        self.present(video_frame)?;
        self.player.commit_frame(frame)?;
        Ok(())
    }

    fn present(&self, frame: VideoFrame) -> Result<(), MediaError> {
        let result = draw_frame_to_canvas(&self.canvas, &frame);
        frame.close();
        result
    }
}
