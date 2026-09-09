//! Core media types. Frame index is primary; time is derived.

/// Zero-based presentation frame index.
pub type FrameId = u64;

/// Media timestamp in seconds (f64 for UI; prefer [`Timestamp::from_ticks`] internally).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Timestamp {
    /// Presentation time in seconds.
    pub seconds: f64,
}

impl Timestamp {
    pub fn from_ticks(ticks: i64, timescale: u64) -> Self {
        let timescale = timescale.max(1) as f64;
        Self {
            seconds: ticks as f64 / timescale,
        }
    }

    pub fn from_seconds(seconds: f64) -> Self {
        Self { seconds }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VideoCodec {
    /// H.264 / AVC (`avc1`), AVCC length-prefixed NALs.
    Avc,
    /// H.265 / HEVC (`hvc1` / `hev1`).
    Hevc,
    /// Recognized but not yet wired for decode.
    Other,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodecConfig {
    pub codec: VideoCodec,
    /// WebCodecs `VideoDecoderConfig.codec` string when known (e.g. `avc1.64001f`).
    pub codec_string: Option<String>,
    /// Extradata for WebCodecs `description` (e.g. full `avcC` box payload).
    pub description: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct VideoMetadata {
    pub width: u32,
    pub height: u32,
    pub timescale: u64,
    /// Track duration in timescale ticks.
    pub duration_ticks: u64,
    pub frame_count: u64,
    /// Average fps from duration/frame_count when available.
    pub fps: Option<f64>,
    pub codec: CodecConfig,
}

impl VideoMetadata {
    pub fn duration(&self) -> Timestamp {
        Timestamp::from_ticks(self.duration_ticks as i64, self.timescale)
    }
}

/// One encoded access unit ready for a decoder (WebCodecs `EncodedVideoChunk`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EncodedSample {
    pub frame: FrameId,
    /// Position in the track's decode-order sample list.
    pub decode_order: usize,
    pub is_keyframe: bool,
    /// Decode timestamp in track timescale ticks.
    pub decode_timestamp_ticks: i64,
    /// Composition (presentation) timestamp in track timescale ticks.
    pub composition_timestamp_ticks: i64,
    pub duration_ticks: u64,
    pub timescale: u64,
    /// Raw sample bytes (AVCC for AVC).
    pub data: Vec<u8>,
}

impl EncodedSample {
    pub fn composition_timestamp(&self) -> Timestamp {
        Timestamp::from_ticks(self.composition_timestamp_ticks, self.timescale)
    }
}
