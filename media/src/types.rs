//! Core media types. Frame index is primary; time is derived.

/// Zero-based presentation frame index.
pub type FrameId = u64;

/// Max GOP length (frames) still treated as short/fixed for scrub policy.
pub const SHORT_GOP_MAX_LEN: u32 = 30;
/// Max (max−min) GOP length spread allowed inside [`GopProfile::ShortFixed`].
pub const SHORT_GOP_MAX_SPREAD: u32 = 2;
/// Default dense-cache RAM budget (decoded `VideoFrame` estimate).
/// Frame count = budget / (w×h×1.5), clamped by [`MAX_SCRUB_CACHE_FRAMES`].
pub const DEFAULT_SCRUB_CACHE_MIB: usize = 1280;
/// Assumed bytes per pixel for a decoded frame (YUV 4:2:0 / NV12 planes).
pub const DECODED_BYTES_PER_PIXEL_NUM: u64 = 3;
pub const DECODED_BYTES_PER_PIXEL_DEN: u64 = 2;
/// Floor so tiny resolutions / tiny budgets still keep a usable ring.
pub const MIN_SCRUB_CACHE_FRAMES: usize = 8;
/// Ceiling so tiny coded sizes cannot explode residency (decoder pool / WASM).
pub const MAX_SCRUB_CACHE_FRAMES: usize = 512;
/// Extra frames beyond extent edge when gap-fill re-arms ([`DenseFrameCache::no_gaps`]).
pub const SCRUB_CACHE_EDGE_MARGIN: u64 = 24;

/// Interim scrub canvas frame while `target` is not yet exact-cached.
///
/// Prefer `cached` when it is at least as close as `key` (tie → cache, already
/// decoded). Key only when strictly closer — avoids key-flicker across holes.
pub fn scrub_preview_choice(
    target: FrameId,
    key: Option<FrameId>,
    cached: Option<FrameId>,
) -> Option<FrameId> {
    match (key, cached) {
        (Some(k), Some(c)) => {
            if c.abs_diff(target) <= k.abs_diff(target) {
                Some(c)
            } else {
                Some(k)
            }
        }
        (Some(k), None) => Some(k),
        (None, Some(c)) => Some(c),
        (None, None) => None,
    }
}

/// Estimated decoded-frame size in bytes (coded width × height × 1.5).
pub fn estimated_decoded_frame_bytes(width: u32, height: u32) -> u64 {
    let w = width.max(1) as u64;
    let h = height.max(1) as u64;
    (w * h * DECODED_BYTES_PER_PIXEL_NUM) / DECODED_BYTES_PER_PIXEL_DEN
}

/// Dense scrub-cache frame cap from a MiB budget and coded resolution.
///
/// Profile no longer shrinks the budget — only fill policy (All-Intra skips gap-fill).
/// Whole short clips stay fully resident when they fit under the byte cap.
pub fn scrub_cache_frame_budget(
    frame_count: u64,
    width: u32,
    height: u32,
    max_mib: usize,
) -> usize {
    let n = frame_count as usize;
    if n == 0 {
        return 1;
    }
    let per = estimated_decoded_frame_bytes(width, height).max(1);
    let budget_bytes = (max_mib as u64).saturating_mul(1024 * 1024);
    let by_bytes = (budget_bytes / per) as usize;
    by_bytes
        .max(MIN_SCRUB_CACHE_FRAMES)
        .min(MAX_SCRUB_CACHE_FRAMES)
        .min(n)
        .max(1)
}

/// Keyframe spacing class used to pick scrub fill policy (not the byte budget).
///
/// Derived from sync-sample spacing after open (and after AVC sync verify).
/// Not a mediabunny concept — local policy for densify on/off.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GopProfile {
    /// Every sample is a sync (All-Intra / GOP = 1).
    AllIntra,
    /// Fixed or near-fixed short GOPs (`max ≤ 30`, spread ≤ 2).
    ShortFixed {
        /// Characteristic GOP length (observed max within the short band).
        len: u32,
    },
    /// Long, variable, or sparse keyframes.
    Long { median: u32, max: u32 },
}

impl GopProfile {
    /// Short label for status / demo UI (`all-intra`, `GOP 12`, `GOP ~250`).
    pub fn label(&self) -> String {
        match self {
            Self::AllIntra => "all-intra".into(),
            Self::ShortFixed { len } => format!("GOP {len}"),
            Self::Long { median, max } if *median == *max => format!("GOP ~{max}"),
            Self::Long { median, max } => format!("GOP ~{median} (max {max})"),
        }
    }
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
    /// Keyframe spacing class — drives scrub fill policy (All-Intra skips densify).
    pub gop_profile: GopProfile,
}

impl VideoMetadata {
    pub fn duration(&self) -> Timestamp {
        Timestamp::from_ticks(self.duration_ticks as i64, self.timescale)
    }

    /// Dense scrub-cache frame cap from [`DEFAULT_SCRUB_CACHE_MIB`] and coded size.
    pub fn scrub_cache_frame_budget(&self) -> usize {
        scrub_cache_frame_budget(
            self.frame_count,
            self.width,
            self.height,
            DEFAULT_SCRUB_CACHE_MIB,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn estimated_frame_bytes_yuv420() {
        assert_eq!(
            estimated_decoded_frame_bytes(1920, 1080),
            1920 * 1080 * 3 / 2
        );
        assert_eq!(estimated_decoded_frame_bytes(320, 240), 320 * 240 * 3 / 2);
    }

    #[test]
    fn budget_scales_with_resolution() {
        // Same MiB → fewer frames at higher resolution.
        let fhd = scrub_cache_frame_budget(10_000, 1920, 1080, DEFAULT_SCRUB_CACHE_MIB);
        let uhd = scrub_cache_frame_budget(10_000, 3840, 2160, DEFAULT_SCRUB_CACHE_MIB);
        assert!(fhd > uhd, "fhd={fhd} uhd={uhd}");
        let fhd_bytes = estimated_decoded_frame_bytes(1920, 1080);
        let expect_fhd = ((DEFAULT_SCRUB_CACHE_MIB as u64 * 1024 * 1024) / fhd_bytes) as usize;
        assert_eq!(fhd, expect_fhd.min(MAX_SCRUB_CACHE_FRAMES));
        assert!(uhd < fhd);
    }

    #[test]
    fn budget_short_clip_fits_entirely() {
        assert_eq!(
            scrub_cache_frame_budget(48, 320, 240, DEFAULT_SCRUB_CACHE_MIB),
            48
        );
    }

    #[test]
    fn budget_respects_max_frames_ceiling() {
        // Tiny resolution would otherwise allow thousands of frames.
        assert_eq!(
            scrub_cache_frame_budget(10_000, 64, 64, 256),
            MAX_SCRUB_CACHE_FRAMES
        );
    }

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
        };
        assert_eq!(
            meta.scrub_cache_frame_budget(),
            scrub_cache_frame_budget(10_000, 1920, 1080, DEFAULT_SCRUB_CACHE_MIB)
        );
    }

    #[test]
    fn scrub_preview_prefers_closer_cache_over_key() {
        // Hole at 50; cache at 40; key at 0 → cache wins.
        assert_eq!(
            scrub_preview_choice(50, Some(0), Some(40)),
            Some(40)
        );
        // Key at 48 closer than cache at 20 → key.
        assert_eq!(
            scrub_preview_choice(50, Some(48), Some(20)),
            Some(48)
        );
        // Tie → cache (no decode flicker).
        assert_eq!(
            scrub_preview_choice(50, Some(40), Some(60)),
            Some(60)
        );
        assert_eq!(scrub_preview_choice(50, Some(0), None), Some(0));
        assert_eq!(scrub_preview_choice(50, None, Some(44)), Some(44));
    }

    #[test]
    fn gop_profile_label() {
        assert_eq!(GopProfile::AllIntra.label(), "all-intra");
        assert_eq!(GopProfile::ShortFixed { len: 12 }.label(), "GOP 12");
        assert_eq!(
            GopProfile::Long {
                median: 200,
                max: 250
            }
            .label(),
            "GOP ~200 (max 250)"
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
    pub data: Vec<u8>,
}

impl EncodedSample {
    pub fn composition_timestamp(&self) -> Timestamp {
        Timestamp::from_ticks(self.composition_timestamp_ticks, self.timescale)
    }
}
