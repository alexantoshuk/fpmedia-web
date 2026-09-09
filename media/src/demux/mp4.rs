//! MP4 / ISOBMFF demux via `re_mp4`.

use re_mp4::{Mp4, TrackKind};

use crate::error::MediaError;
use crate::frame_index::{FrameEntry, FrameIndex};
use crate::types::{
    CodecConfig, EncodedSample, FrameId, VideoCodec, VideoMetadata,
};

/// Demuxed H.264/H.265 (etc.) video track with a presentation-order frame index.
#[derive(Clone, Debug)]
pub struct Mp4VideoTrack {
    bytes: Vec<u8>,
    metadata: VideoMetadata,
    index: FrameIndex,
}

impl Mp4VideoTrack {
    pub fn metadata(&self) -> &VideoMetadata {
        &self.metadata
    }

    pub fn index(&self) -> &FrameIndex {
        &self.index
    }

    pub fn frame_count(&self) -> u64 {
        self.metadata.frame_count
    }

    pub fn encoded_sample(&self, frame: FrameId) -> Result<EncodedSample, MediaError> {
        let entry = self
            .index
            .get(frame)
            .ok_or(MediaError::FrameOutOfRange(frame, self.frame_count()))?;
        let range = entry.byte_range();
        let data = self
            .bytes
            .get(range.clone())
            .ok_or_else(|| {
                MediaError::Mp4(format!(
                    "sample byte range {range:?} outside file (len={})",
                    self.bytes.len()
                ))
            })?
            .to_vec();

        Ok(EncodedSample {
            frame: entry.frame,
            decode_order: entry.decode_order,
            is_keyframe: entry.is_sync,
            decode_timestamp_ticks: entry.decode_timestamp_ticks,
            composition_timestamp_ticks: entry.composition_timestamp_ticks,
            duration_ticks: entry.duration_ticks,
            timescale: entry.timescale,
            data,
        })
    }

    /// Encoded samples from nearest keyframe through `frame`, decode order.
    pub fn decode_cascade(&self, frame: FrameId) -> Result<Vec<EncodedSample>, MediaError> {
        let window = self
            .index
            .decode_window(frame)
            .ok_or(MediaError::FrameOutOfRange(frame, self.frame_count()))?;
        window
            .into_iter()
            .map(|f| self.encoded_sample(f))
            .collect()
    }

    /// Copy only samples with `decode_order` in `from_do..=to_do` (inclusive), decode order.
    pub fn samples_decode_range(
        &self,
        from_do: usize,
        to_do: usize,
    ) -> Result<Vec<EncodedSample>, MediaError> {
        if from_do > to_do {
            return Ok(Vec::new());
        }
        let mut entries: Vec<_> = self
            .index
            .frames()
            .iter()
            .filter(|e| e.decode_order >= from_do && e.decode_order <= to_do)
            .collect();
        entries.sort_by_key(|e| e.decode_order);
        entries
            .into_iter()
            .map(|e| self.encoded_sample(e.frame))
            .collect()
    }

    /// Next `limit` samples after `after_do` within the GOP that starts at `key_do`.
    pub fn samples_after_in_gop(
        &self,
        key_do: usize,
        after_do: usize,
        limit: usize,
    ) -> Result<Vec<EncodedSample>, MediaError> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let next_key_do = self
            .index
            .frames()
            .iter()
            .filter(|e| e.is_sync && e.decode_order > key_do)
            .map(|e| e.decode_order)
            .min();
        let mut entries: Vec<_> = self
            .index
            .frames()
            .iter()
            .filter(|e| {
                e.decode_order > after_do
                    && next_key_do
                        .map(|nk| e.decode_order < nk)
                        .unwrap_or(true)
            })
            .collect();
        entries.sort_by_key(|e| e.decode_order);
        entries.truncate(limit);
        entries
            .into_iter()
            .map(|e| self.encoded_sample(e.frame))
            .collect()
    }
}

/// Open an MP4 from an owned byte buffer (keeps bytes for sample slicing).
pub fn open_mp4_bytes(bytes: Vec<u8>) -> Result<Mp4VideoTrack, MediaError> {
    let mp4 = Mp4::read_bytes(&bytes)?;
    build_track(mp4, bytes)
}

/// Open an MP4 from a filesystem path (native / tests).
#[cfg(not(target_arch = "wasm32"))]
pub fn open_mp4_path(path: impl AsRef<std::path::Path>) -> Result<Mp4VideoTrack, MediaError> {
    let bytes = std::fs::read(path)?;
    open_mp4_bytes(bytes)
}

fn build_track(mp4: Mp4, bytes: Vec<u8>) -> Result<Mp4VideoTrack, MediaError> {
    let track = mp4
        .tracks()
        .values()
        .find(|t| t.kind == Some(TrackKind::Video))
        .ok_or(MediaError::NoVideoTrack)?;

    if track.samples.is_empty() {
        return Err(MediaError::EmptyTrack);
    }

    let codec_string = track.codec_string(&mp4);
    let description = track
        .raw_codec_config(&mp4)
        .ok_or(MediaError::UnsupportedCodec)?;

    let codec = classify_codec(codec_string.as_deref());

    let entries: Vec<FrameEntry> = track
        .samples
        .iter()
        .enumerate()
        .map(|(decode_order, sample)| FrameEntry {
            frame: 0,
            decode_order,
            is_sync: sample.is_sync,
            decode_timestamp_ticks: sample.decode_timestamp,
            composition_timestamp_ticks: sample.composition_timestamp,
            duration_ticks: sample.duration,
            timescale: sample.timescale,
            offset: sample.offset,
            size: sample.size,
        })
        .collect();

    let index = FrameIndex::from_entries(entries);
    let frame_count = index.len() as u64;

    let fps = if track.duration > 0 && frame_count > 0 {
        Some(frame_count as f64 * track.timescale as f64 / track.duration as f64)
    } else {
        None
    };

    let metadata = VideoMetadata {
        width: u32::from(track.width),
        height: u32::from(track.height),
        timescale: track.timescale,
        duration_ticks: track.duration,
        frame_count,
        fps,
        codec: CodecConfig {
            codec,
            codec_string,
            description,
        },
    };

    Ok(Mp4VideoTrack {
        bytes,
        metadata,
        index,
    })
}

fn classify_codec(codec_string: Option<&str>) -> VideoCodec {
    match codec_string {
        Some(s) if s.starts_with("avc1") || s.starts_with("avc3") => VideoCodec::Avc,
        Some(s) if s.starts_with("hvc1") || s.starts_with("hev1") => VideoCodec::Hevc,
        _ => VideoCodec::Other,
    }
}
