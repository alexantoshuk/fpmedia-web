//! Player state machine over a demuxed video track.
//!
//! Positions on a frame and exposes the encoded cascade a decoder consumes.
//! Frame state is updated only through explicit methods; callers that decode
//! asynchronously should build a window first, then commit on success.

use std::rc::Rc;

use crate::demux::Mp4VideoTrack;
use crate::error::MediaError;
use crate::types::{EncodedSample, FrameId, Timestamp, VideoMetadata};

#[derive(Clone, Debug, PartialEq)]
pub struct PlayerState {
    pub frame: FrameId,
    pub timestamp: Timestamp,
    pub playing: bool,
    /// When true, playback wraps to frame 0 at end (HTML `<video loop>`).
    pub looping: bool,
    pub frame_count: u64,
}

/// Encoded samples to feed a decoder for an exact frame (keyframe cascade).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DecodeWindow {
    pub target_frame: FrameId,
    /// Samples in decode order (keyframe first).
    pub samples: Vec<EncodedSample>,
}

/// Frame-accurate player over [`Mp4VideoTrack`].
#[derive(Debug)]
pub struct Player {
    track: Rc<Mp4VideoTrack>,
    frame: FrameId,
    playing: bool,
    looping: bool,
}

impl Player {
    pub fn open_mp4_bytes(bytes: Vec<u8>) -> Result<Self, MediaError> {
        let track = crate::demux::open_mp4_bytes(bytes)?;
        Self::from_track(track)
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub fn open_mp4_path(path: impl AsRef<std::path::Path>) -> Result<Self, MediaError> {
        let track = crate::demux::open_mp4_path(path)?;
        Self::from_track(track)
    }

    pub fn from_track(track: Mp4VideoTrack) -> Result<Self, MediaError> {
        if track.frame_count() == 0 {
            return Err(MediaError::EmptyTrack);
        }
        Ok(Self {
            track: Rc::new(track),
            frame: 0,
            playing: false,
            looping: false,
        })
    }

    pub fn metadata(&self) -> &VideoMetadata {
        self.track.metadata()
    }

    pub fn track(&self) -> &Mp4VideoTrack {
        &self.track
    }

    pub fn track_rc(&self) -> Rc<Mp4VideoTrack> {
        Rc::clone(&self.track)
    }

    pub fn state(&self) -> PlayerState {
        let timestamp = self
            .track
            .index()
            .get(self.frame)
            .map(|e| e.composition_timestamp())
            .unwrap_or(Timestamp::from_seconds(0.0));
        PlayerState {
            frame: self.frame,
            timestamp,
            playing: self.playing,
            looping: self.looping,
            frame_count: self.track.frame_count(),
        }
    }

    pub fn play(&mut self) {
        self.playing = true;
    }

    pub fn pause(&mut self) {
        self.playing = false;
    }

    pub fn set_loop(&mut self, looping: bool) {
        self.looping = looping;
    }

    pub fn is_looping(&self) -> bool {
        self.looping
    }

    /// Build a decode cascade for `frame` without changing current position.
    pub async fn window_at(&self, frame: FrameId) -> Result<DecodeWindow, MediaError> {
        self.ensure_in_range(frame)?;
        Ok(DecodeWindow {
            target_frame: frame,
            samples: self.track.decode_cascade(frame).await?,
        })
    }

    /// Commit current position to `frame` (call after a successful decode).
    /// Does not change [`Self::playing`] — playback loop relies on that.
    pub fn commit_frame(&mut self, frame: FrameId) -> Result<(), MediaError> {
        self.ensure_in_range(frame)?;
        self.frame = frame;
        Ok(())
    }

    /// Seek to presentation frame `frame` and return the decode cascade.
    pub async fn seek_frame(&mut self, frame: FrameId) -> Result<DecodeWindow, MediaError> {
        let window = self.window_at(frame).await?;
        self.pause();
        self.commit_frame(frame)?;
        Ok(window)
    }

    pub async fn next_frame(&mut self) -> Result<DecodeWindow, MediaError> {
        let next = self.frame.saturating_add(1);
        if next >= self.track.frame_count() {
            return Err(MediaError::FrameOutOfRange(next, self.track.frame_count()));
        }
        self.seek_frame(next).await
    }

    pub async fn previous_frame(&mut self) -> Result<DecodeWindow, MediaError> {
        if self.frame == 0 {
            return Err(MediaError::FrameOutOfRange(0, self.track.frame_count()));
        }
        self.seek_frame(self.frame - 1).await
    }

    /// Current frame's encoded sample (not the full cascade).
    pub async fn current_sample(&self) -> Result<EncodedSample, MediaError> {
        self.track.encoded_sample(self.frame).await
    }

    fn ensure_in_range(&self, frame: FrameId) -> Result<(), MediaError> {
        if frame >= self.track.frame_count() {
            Err(MediaError::FrameOutOfRange(frame, self.track.frame_count()))
        } else {
            Ok(())
        }
    }
}
