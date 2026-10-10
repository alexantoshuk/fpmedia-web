//! Core media types. Frame index is primary; time is derived.
//! Clip policies / budgets: [`crate::policy`].

use crate::policy::{
    GopProfile, PlayPolicy, ResolutionClass, ScrubPolicy, SettlePolicy, scrub_cache_frame_budget,
};

/// Zero-based presentation frame index.
pub type FrameId = u64;

/// Merge sorted unique frame ids into inclusive `[lo, hi]` runs.
///
/// Used by the demo ready strip so Play can show densify coverage without
/// one DOM node per frame (was: skip marks while playing → strip vanished).
pub fn coalesce_frame_runs(ids: impl IntoIterator<Item = FrameId>) -> Vec<(FrameId, FrameId)> {
    let mut out = Vec::new();
    let mut iter = ids.into_iter();
    let Some(mut lo) = iter.next() else {
        return out;
    };
    let mut hi = lo;
    for id in iter {
        if id == hi.saturating_add(1) {
            hi = id;
        } else {
            out.push((lo, hi));
            lo = id;
            hi = id;
        }
    }
    out.push((lo, hi));
    out
}

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
    /// H.265 / HEVC (`hvc1` / `hev1`). Requires cargo feature `hevc` to open.
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
    /// Keyframe spacing class — drives [`ScrubPolicy`] (densify / drag mode).
    pub gop_profile: GopProfile,
    /// Embedded tape start from QuickTime `tmcd` (if present).
    /// Current tape TC = [`timecode::Timecode::from_start`]`(frame_id, start)`.
    pub timecode_start: Option<timecode::Timecode>,
}

impl VideoMetadata {
    pub fn duration(&self) -> Timestamp {
        Timestamp::from_ticks(self.duration_ticks as i64, self.timescale)
    }

    /// Play knobs (primary) — from coded height. Hd ≡ historical FHD defaults.
    pub fn play_policy(&self) -> PlayPolicy {
        PlayPolicy::from_resolution(self.height)
    }

    pub fn resolution_class(&self) -> ResolutionClass {
        ResolutionClass::from_height(self.height)
    }

    /// Scrub knobs (secondary) — `GopProfile` → soft-drag / densify / budgets.
    pub fn scrub_policy(&self) -> ScrubPolicy {
        ScrubPolicy::from_gop_profile(self.gop_profile)
    }

    /// Settle / PlayPrime timing budgets (tertiary). Clip-invariant for now.
    pub fn settle_policy(&self) -> SettlePolicy {
        SettlePolicy::default()
    }

    /// Dense scrub-cache frame cap from policy MiB budget and coded size.
    pub fn scrub_cache_frame_budget(&self) -> usize {
        let mib = self.scrub_policy().scrub_cache_mib;
        scrub_cache_frame_budget(self.frame_count, self.width, self.height, mib)
    }

    /// Absolute frame count of embedded start TC, or `0` (zero-based tape).
    pub fn timecode_origin_frames(&self) -> i64 {
        self.timecode_start.map(|tc| tc.total_frames()).unwrap_or(0)
    }

    /// Rate for SMPTE display: `tmcd` rate if present, else nominal from [`Self::fps`].
    pub fn timecode_rate(&self) -> Option<timecode::FrameRate> {
        if let Some(tc) = self.timecode_start {
            return Some(tc.rate());
        }
        self.fps
            .and_then(|fps| timecode::FrameRate::from_float(fps, false))
    }

    /// Tape (or zero) timecode for presentation `frame_id`.
    ///
    /// Hot path: `origin_frames + frame_id` then format — same math as
    /// [`timecode::Timecode::from_start`], without rebuilding the start value.
    pub fn timecode_at(&self, frame_id: u64) -> Option<timecode::Timecode> {
        let rate = self.timecode_rate()?;
        let abs = self
            .timecode_origin_frames()
            .saturating_add(frame_id as i64);
        Some(timecode::Timecode::from_frames(abs, rate))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::DEFAULT_SCRUB_CACHE_MIB;

    #[test]
    fn metadata_budget_uses_defaults() {
        let meta = VideoMetadata {
            width: 1920,
            height: 1080,
            timescale: 24,
            duration_ticks: 240,
            frame_count: 10_000,
            fps: Some(24.0),
            codec: CodecConfig {
                codec: VideoCodec::Avc,
                codec_string: None,
                description: Vec::new(),
            },
            gop_profile: GopProfile::ShortFixed { len: 12 },
            timecode_start: None,
        };
        assert_eq!(
            meta.scrub_cache_frame_budget(),
            scrub_cache_frame_budget(10_000, 1920, 1080, DEFAULT_SCRUB_CACHE_MIB)
        );
    }

    #[test]
    fn timecode_at_zero_and_start_offset() {
        let mut meta = VideoMetadata {
            width: 320,
            height: 240,
            timescale: 24,
            duration_ticks: 240,
            frame_count: 48,
            fps: Some(24.0),
            codec: CodecConfig {
                codec: VideoCodec::Avc,
                codec_string: None,
                description: Vec::new(),
            },
            gop_profile: GopProfile::ShortFixed { len: 12 },
            timecode_start: None,
        };
        assert_eq!(meta.timecode_origin_frames(), 0);
        assert_eq!(
            meta.timecode_at(24).map(|tc| tc.to_string()).as_deref(),
            Some("00:00:01:00")
        );

        meta.timecode_start =
            Some(timecode::Timecode::parse("10:00:00:00", timecode::FrameRate::Fps24).unwrap());
        assert_eq!(meta.timecode_origin_frames(), 864_000);
        assert_eq!(
            meta.timecode_at(24).map(|tc| tc.to_string()).as_deref(),
            Some("10:00:01:00")
        );
    }

    #[test]
    fn coalesce_frame_runs_merges_contiguous() {
        assert!(coalesce_frame_runs([] as [FrameId; 0]).is_empty());
        assert_eq!(coalesce_frame_runs([3u64]), vec![(3, 3)]);
        assert_eq!(
            coalesce_frame_runs([0u64, 1, 2, 5, 6, 10]),
            vec![(0, 2), (5, 6), (10, 10)]
        );
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
    ///
    /// [`bytes::Bytes`] so cache hits share the window buffer with decode
    /// (`mediabunny:` packet bytes stay one allocation into the chunk).
    pub data: bytes::Bytes,
}

impl EncodedSample {
    pub fn composition_timestamp(&self) -> Timestamp {
        Timestamp::from_ticks(self.composition_timestamp_ticks, self.timescale)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AudioCodec {
    /// AAC-LC / HE-AAC in MP4 (`mp4a`).
    Aac,
    /// Uncompressed / G.711 PCM (`in24`, `sowt`, …) — no WebCodecs decoder.
    Pcm,
    Other,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AudioCodecConfig {
    pub codec: AudioCodec,
    /// WebCodecs `AudioDecoderConfig.codec` (e.g. `mp4a.40.2`) or PCM string (`pcm-s24be`).
    pub codec_string: Option<String>,
    /// AudioSpecificConfig bytes for `description` (rebuilt from esds when needed).
    pub description: Vec<u8>,
    /// Set when [`AudioCodec::Pcm`] — used by `audio::pcm` to convert packets.
    pub pcm: Option<crate::audio::pcm::PcmFormat>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct AudioMetadata {
    pub sample_rate: u32,
    pub number_of_channels: u32,
    pub timescale: u64,
    pub duration_ticks: u64,
    pub sample_count: u64,
    pub codec: AudioCodecConfig,
}

impl AudioMetadata {
    pub fn duration(&self) -> Timestamp {
        Timestamp::from_ticks(self.duration_ticks as i64, self.timescale)
    }
}

/// One encoded audio access unit (WebCodecs `EncodedAudioChunk`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EncodedAudioSample {
    pub sample: u64,
    pub is_keyframe: bool,
    pub decode_timestamp_ticks: i64,
    pub composition_timestamp_ticks: i64,
    pub duration_ticks: u64,
    pub timescale: u64,
    pub data: bytes::Bytes,
}

impl EncodedAudioSample {
    pub fn composition_timestamp(&self) -> Timestamp {
        Timestamp::from_ticks(self.composition_timestamp_ticks, self.timescale)
    }
}

/// Rebuild a 2-byte AAC AudioSpecificConfig from esds fields.
///
/// `re_mp4` parses ASC into profile/freq/chan but does not expose raw bytes for
/// `mp4a` (`raw_codec_config` is `None`). WebCodecs still wants `description`.
pub fn build_aac_asc(profile: u8, freq_index: u8, chan_conf: u8) -> Vec<u8> {
    let profile = profile.max(1);
    let byte0 = (profile << 3) | ((freq_index >> 1) & 0x07);
    let byte1 = ((freq_index & 1) << 7) | ((chan_conf & 0x0F) << 3);
    vec![byte0, byte1]
}

#[cfg(test)]
mod aac_asc_tests {
    use super::build_aac_asc;

    #[test]
    fn aac_lc_48k_stereo_asc() {
        // profile=2 (AAC-LC), freq_index=3 (48k), chan=2
        assert_eq!(build_aac_asc(2, 3, 2), vec![0x11, 0x90]);
    }
}
