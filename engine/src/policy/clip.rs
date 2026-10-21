//! Clip policies (Gop / Scrub / Play / Settle / Thumb) + cache budgets and scrub pick math.

use crate::types::FrameId;

/// Max GOP length (frames) still treated as short for scrub policy.
///
/// Scene cuts shorten some GOPs (large max−min spread) but `max ≤` this still
/// counts as short-IP — do **not** gate on spread (was false-`Long` on ShotGrid).
pub const SHORT_GOP_MAX_LEN: u32 = 30;
/// Default dense-cache marker budget (decoded-frame **estimate** only — store holds
/// markers, not pixels). Frame count = budget / (w×h×1.5), clamped by
/// [`MAX_SCRUB_CACHE_FRAMES`]. Prefer [`ScrubPolicy::scrub_cache_mib`].
pub const DEFAULT_SCRUB_CACHE_MIB: usize = 8192;
/// Reference live blit count at 1280×720 for Short ([`DEFAULT_SCRUB_BLIT_MIB_SHORT`]).
/// Prefer [`scrub_blit_frame_budget`] from policy MiB × canvas buffer — do not raise this alone.
pub const SCRUB_BLIT_LRU: usize = 96;
/// Short / All-Intra GPU MiB for live densify `ImageBitmap`s (~96 frames at 1280×720 RGBA).
pub const DEFAULT_SCRUB_BLIT_MIB_SHORT: usize = 338;
/// Long-GOP GPU MiB for live densify bitmaps (wider mid-timeline window than Short).
pub const DEFAULT_SCRUB_BLIT_MIB_LONG: usize = 512;
/// Assumed bytes per pixel for a live densify `ImageBitmap` (RGBA GPU texture).
pub const BLIT_BYTES_PER_PIXEL: u64 = 4;
/// Floor so tiny viewports / tiny budgets still keep a usable blit ring.
pub const MIN_SCRUB_BLIT_FRAMES: usize = 24;
/// Ceiling so tiny CSS buffers cannot explode GPU residency.
pub const MAX_SCRUB_BLIT_FRAMES: usize = 192;
/// Assumed bytes per pixel for a decoded frame (YUV 4:2:0 / NV12 planes).
pub const DECODED_BYTES_PER_PIXEL_NUM: u64 = 3;
pub const DECODED_BYTES_PER_PIXEL_DEN: u64 = 2;
/// Floor so tiny resolutions / tiny budgets still keep a usable ring.
pub const MIN_SCRUB_CACHE_FRAMES: usize = 8;
/// Ceiling on Covered markers (gap geometry). Markers are cheap; keep a hard
/// cap so pathological tiny coded sizes cannot densify forever. Exact blit
/// stays on [`MAX_SCRUB_BLIT_FRAMES`] / `blit_mib`.
pub const MAX_SCRUB_CACHE_FRAMES: usize = 4096;
/// Floor for [`ScrubPolicy::margin_left`] (timeline earlier / lower FrameId).
pub const EDGE_MARGIN_LEFT_MIN: u64 = 8;
/// Ceiling for [`ScrubPolicy::margin_left`] — keep below ~¼ typical blit window.
pub const EDGE_MARGIN_LEFT_MAX: u64 = 24;
/// Floor for [`ScrubPolicy::margin_right`] (timeline later / higher FrameId).
pub const EDGE_MARGIN_RIGHT_MIN: u64 = 4;

/// Asymmetric edge margins from a GOP length hint (Short `len` / Long `median`).
///
/// Left > right: densify fills a GOP from its keyframe forward, so expanding
/// **behind** needs more lead time (near-edge frames of the prev GOP arrive
/// last). Local invention (mediabunny: no equivalent).
pub fn scrub_edge_margins(gop_hint: u32) -> (u64, u64) {
    let left = u64::from(gop_hint).clamp(EDGE_MARGIN_LEFT_MIN, EDGE_MARGIN_LEFT_MAX);
    let right = (left / 2).max(EDGE_MARGIN_RIGHT_MIN);
    (left, right)
}

/// Gap-fill arming once the dense window is contiguous **and at capacity**.
///
/// - Holes (`!no_gaps`) → always armed (caller).
/// - Under budget → always armed (caller).
/// - Contiguous at capacity → armed when `playhead` is outside `[start, end]`,
///   within `margin_left` of start **and** `start > 0` (can expand behind), or
///   within `margin_right` of end **and** `end < last` (can expand ahead).
///
/// Do **not** arm from a pinned timeline edge (`start == 0` / `end == last`):
/// that kept densify expanding the other side → farthest eviction → thrash.
/// Left margin is typically larger: GOP densify fills key→forward. Local
/// invention (mediabunny: no equivalent).
pub fn scrub_gap_fill_armed(
    no_gaps: bool,
    playhead: FrameId,
    extent: Option<(FrameId, FrameId)>,
    last: FrameId,
    margin_left: u64,
    margin_right: u64,
) -> bool {
    if !no_gaps {
        return true;
    }
    let Some((start, end)) = extent else {
        return true;
    };
    if playhead < start || playhead > end {
        return true;
    }
    let near_left = start > 0 && playhead < start.saturating_add(margin_left);
    let near_right = end < last && playhead > end.saturating_sub(margin_right);
    near_left || near_right
}

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

/// Contain `src` into a max box (hover / filmstrip thumb size).
pub fn hover_thumb_dims(src_w: u32, src_h: u32, max_w: u32, max_h: u32) -> (u32, u32) {
    let src_w = src_w.max(1) as f64;
    let src_h = src_h.max(1) as f64;
    let max_w = max_w.max(1) as f64;
    let max_h = max_h.max(1) as f64;
    let scale = (max_w / src_w).min(max_h / src_h);
    let w = (src_w * scale).round().max(1.0) as u32;
    let h = (src_h * scale).round().max(1.0) as u32;
    (w, h)
}

/// Nearest sync id in `keys` that is not yet `ready`, by `|id − focus|`.
///
/// Used by sparse thumb key fill (`mediabunny:` `canvasesAtTimestamps` pick order
/// is caller-defined; we bias to hover/playhead focus).
pub fn next_missing_sync_key(
    keys: &[FrameId],
    focus: FrameId,
    ready: impl Fn(FrameId) -> bool,
) -> Option<FrameId> {
    keys.iter()
        .copied()
        .filter(|&k| !ready(k))
        .min_by_key(|k| k.abs_diff(focus))
}

/// Dense-cache gap pick for scrub densify.
///
/// **Distance-first:** always the gap with smaller `|gap − playhead|`. Far/edge
/// fills lose to budget eviction anyway — spending densify there first wastes
/// work. `dir` only breaks ties (`>0` → ahead, else behind / PREV-friendly).
///
/// Local invention (predictive densify); mediabunny has no equivalent.
pub fn nearer_gap_with_dir(
    playhead: FrameId,
    behind: Option<FrameId>,
    ahead: Option<FrameId>,
    dir: i8,
) -> Option<FrameId> {
    match (behind, ahead) {
        (Some(b), Some(a)) => match playhead.abs_diff(b).cmp(&playhead.abs_diff(a)) {
            std::cmp::Ordering::Less => Some(b),
            std::cmp::Ordering::Greater => Some(a),
            std::cmp::Ordering::Equal => {
                if dir > 0 {
                    Some(a)
                } else {
                    Some(b)
                }
            }
        },
        (Some(b), None) => Some(b),
        (None, Some(a)) => Some(a),
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
/// Whole short clips stay fully resident when they fit under the byte cap.
/// Prefer [`VideoMetadata::scrub_cache_frame_budget`] (reads [`ScrubPolicy`]).
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
        .clamp(MIN_SCRUB_CACHE_FRAMES, MAX_SCRUB_CACHE_FRAMES)
        .min(n)
        .max(1)
}

/// Live densify `ImageBitmap` cap from a GPU MiB budget and canvas buffer size.
///
/// Aim-centered residency: smaller CSS×DPR buffers → more frames in the same
/// MiB; fullscreen → fewer. Tune [`ScrubPolicy::blit_mib`] per profile — do not
/// raise [`SCRUB_BLIT_LRU`] alone. `mediabunny:` CanvasSink `width`/`height`/`fit`
/// + `poolSize` (sized canvases, bounded VRAM).
pub fn scrub_blit_frame_budget(width: u32, height: u32, max_mib: usize) -> usize {
    let w = width.max(1) as u64;
    let h = height.max(1) as u64;
    let per = (w * h * BLIT_BYTES_PER_PIXEL).max(1);
    let budget_bytes = (max_mib as u64).saturating_mul(1024 * 1024);
    let by_bytes = (budget_bytes / per) as usize;
    by_bytes
        .clamp(MIN_SCRUB_BLIT_FRAMES, MAX_SCRUB_BLIT_FRAMES)
        .max(1)
}

/// How many Exact (`ImageBitmap`) slots to free so Play can hold `play_vf_count`
/// decoded `VideoFrame`s (YUV) without fighting densify VRAM.
///
/// Converts Play VF byte estimate → blit RGBA slots at the canvas buffer size.
/// Local invention (mediabunny: no Play↔CanvasSink eviction couple).
pub fn exact_frames_to_free_for_play_vram(
    play_vf_count: usize,
    coded_width: u32,
    coded_height: u32,
    blit_width: u32,
    blit_height: u32,
) -> usize {
    if play_vf_count == 0 {
        return 0;
    }
    let play_bytes = estimated_decoded_frame_bytes(coded_width, coded_height)
        .saturating_mul(play_vf_count as u64);
    let blit_w = blit_width.max(1) as u64;
    let blit_h = blit_height.max(1) as u64;
    let blit_per = (blit_w * blit_h * BLIT_BYTES_PER_PIXEL).max(1);
    play_bytes.div_ceil(blit_per) as usize
}

/// Exact cap while playing: keep near-playhead bitmaps, free `reserve` farthest.
pub fn scrub_blit_cap_during_play(full_cap: usize, reserve_frames: usize) -> usize {
    full_cap.saturating_sub(reserve_frames).max(1)
}

/// Keyframe spacing class used to pick scrub fill policy (not the byte budget).
///
/// Derived from sync-sample spacing after open (and after AVC sync verify).
/// Not a mediabunny concept — local policy for densify on/off + drag mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GopProfile {
    /// Every sample is a sync (All-Intra / GOP = 1).
    AllIntra,
    /// Short GOPs (`max ≤ `[`SHORT_GOP_MAX_LEN`]) — includes scene-cut spread.
    ShortFixed {
        /// Characteristic GOP length (observed max within the short band).
        len: u32,
    },
    /// Long, variable, or sparse keyframes (`max > `[`SHORT_GOP_MAX_LEN`]).
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

/// Runtime gate for hover / filmstrip thumbs (`ThumbPreview` sink).
///
/// Compile feature `thumbs` (default on) must also be enabled; `Off` drops the
/// lazy sink. Agreed UX: `On` = play **and** pause (host hides only while
/// scrubbing). `mediabunny:` separate small `CanvasSink` — not the play/scrub sink.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ThumbPolicy {
    /// No thumb sink (drop ThumbCache / future thumb decoder).
    Off,
    /// Enable lazy thumb sink (play+pause; host gates scrub).
    #[default]
    On,
}

/// Runtime gate for the scrub cache plane (densify decoder + Covered/Exact + gray fill).
///
/// `Off` (host has no scrub UI): close the scrub decoder, clear Covered + Exact,
/// refuse densify / gray fill / scrub ingest. Seek / step / Play stay on the main
/// decoder. `mediabunny: src/media-sink.ts mediaSamplesInRange` — a decoder lives
/// only while its sink has a consumer.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ScrubCachePolicy {
    Off,
    #[default]
    On,
}

/// Per-clip scrub knobs derived from [`GopProfile`] (not global constants).
///
/// Local invention (mediabunny: no equivalent). Tune fields per profile; do not
/// raise globals like [`SCRUB_BLIT_LRU`] from a single ShotGrid probe.
///
/// Drag is always blit/aim only for every profile; Exact decode runs on settle
/// (Long key→N per drag tick froze the pointer).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScrubPolicy {
    /// Gap-fill densify (VF → ImageBitmap). Off for All-Intra.
    pub densify: bool,
    /// GPU MiB budget for live densify `ImageBitmap`s (→ [`scrub_blit_frame_budget`]).
    pub blit_mib: usize,
    /// Dense marker store MiB budget (decoded-frame estimate).
    pub scrub_cache_mib: usize,
    /// Re-arm densify within this many frames of extent **start** (earlier / left).
    pub margin_left: u64,
    /// Re-arm densify within this many frames of extent **end** (later / right).
    pub margin_right: u64,
}

impl ScrubPolicy {
    pub fn from_gop_profile(profile: GopProfile) -> Self {
        let (margin_left, margin_right) = match profile {
            GopProfile::AllIntra => scrub_edge_margins(1),
            GopProfile::ShortFixed { len } => scrub_edge_margins(len),
            GopProfile::Long { median, .. } => scrub_edge_margins(median),
        };
        match profile {
            GopProfile::AllIntra => Self {
                densify: false,
                blit_mib: DEFAULT_SCRUB_BLIT_MIB_SHORT,
                scrub_cache_mib: DEFAULT_SCRUB_CACHE_MIB,
                margin_left,
                margin_right,
            },
            GopProfile::ShortFixed { .. } => Self {
                densify: true,
                blit_mib: DEFAULT_SCRUB_BLIT_MIB_SHORT,
                scrub_cache_mib: DEFAULT_SCRUB_CACHE_MIB,
                margin_left,
                margin_right,
            },
            GopProfile::Long { .. } => Self {
                densify: true,
                blit_mib: DEFAULT_SCRUB_BLIT_MIB_LONG,
                scrub_cache_mib: DEFAULT_SCRUB_CACHE_MIB,
                margin_left,
                margin_right,
            },
        }
    }
}

/// Coded-size class for play knobs. Scrub stays on [`ScrubPolicy`] (GOP).
///
/// Product: smooth 1× play is primary; scrub is secondary. **Hd must keep the
/// historical Full-HD defaults** (`PRESENTATION_QUEUE_CAP` / warm-ahead = 4,
/// no `prefer-hardware`) so FHD play+scrub stay unchanged.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResolutionClass {
    /// height &lt; [`UHD_MIN_HEIGHT`] — current Full-HD / below behaviour.
    Hd,
    /// height ≥ [`UHD_MIN_HEIGHT`] (4K+).
    Uhd,
}

/// Minimum coded height for [`ResolutionClass::Uhd`].
pub const UHD_MIN_HEIGHT: u32 = 2160;

/// Play presentation ahead — Hd (= historical `PRESENTATION_QUEUE_CAP`).
pub const PLAY_PRESENTATION_CAP_HD: usize = 4;
/// Play presentation ahead — Uhd (local invention; absorb 4K decode latency).
pub const PLAY_PRESENTATION_CAP_UHD: usize = 10;
/// Decode warm-ahead — Hd ([`PlayPolicy::hd`] / `decode_warm_ahead`).
pub const PLAY_WARM_AHEAD_HD: usize = 4;
/// Decode warm-ahead — Uhd.
pub const PLAY_WARM_AHEAD_UHD: usize = 8;

/// Per-clip step / seek decode knobs. Derived from coded height, not GOP.
///
/// Continuous Play does **not** read these — it uses the mediabunny iterator
/// (`MAX_QUEUE_SIZE = 4`, 2-canvas pool; `decode::webcodecs::PlayIter`).
/// Local invention for Uhd ring/warm (mediabunny has no resolution profile).
/// `prefer_hardware` documents Uhd intent; **main** decoder always probes HW
/// when allowed (`WebCodecsDecoder::create`). Scrub/thumb keep
/// [`PlayPolicy::hd`] + PreferSoftware (mediabunny `decoderOptions.hardwareAcceleration`,
/// PR #406 — dual HW stalls).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PlayPolicy {
    pub class: ResolutionClass,
    /// Paused Next ring (decoded ahead of the playhead for step).
    pub presentation_cap: usize,
    /// Seek/step packet-pump warm frames past the consumer target.
    pub decode_warm_ahead: usize,
    /// Uhd documents HW intent; scrub densify uses [`Self::hd`] + PreferSoftware.
    /// Main play probes HW via `create` regardless of Hd/Uhd.
    pub prefer_hardware: bool,
}

impl PlayPolicy {
    /// Historical Full-HD queue/warm defaults — scrub/thumb knobs (never steal HW).
    pub fn hd() -> Self {
        Self {
            class: ResolutionClass::Hd,
            presentation_cap: PLAY_PRESENTATION_CAP_HD,
            decode_warm_ahead: PLAY_WARM_AHEAD_HD,
            prefer_hardware: false,
        }
    }

    pub fn from_resolution(height: u32) -> Self {
        if height >= UHD_MIN_HEIGHT {
            Self {
                class: ResolutionClass::Uhd,
                presentation_cap: PLAY_PRESENTATION_CAP_UHD,
                decode_warm_ahead: PLAY_WARM_AHEAD_UHD,
                prefer_hardware: true,
            }
        } else {
            Self::hd()
        }
    }

    pub fn label(self) -> &'static str {
        match self.class {
            ResolutionClass::Hd => "hd",
            ResolutionClass::Uhd => "uhd",
        }
    }
}

/// Densify yield before Exact — first window (ms).
pub const SETTLE_DENSIFY_YIELD_MS: f64 = 250.0;
/// Densify yield retry after re-abort (ms).
pub const SETTLE_DENSIFY_YIELD_RETRY_MS: f64 = 250.0;
/// Floor for one-GOP sample warm on Exact settle / cold PlayPrime.
pub const SETTLE_GOP_SAMPLE_LEAD_MIN: u64 = 8;
/// Ceiling for one-GOP sample warm (was fixed-24 stall on long/cold GOP).
pub const SETTLE_GOP_SAMPLE_LEAD_MAX: u64 = 24;
/// Index / sample prefetch lead for PlayPrime + open warm (`mdat` frames).
/// Shared with demux `warm_open_playback_bytes` — keep one number.
pub const SETTLE_PLAY_INDEX_LEAD: u64 = 96;
/// fMP4: max frames to *await* past current index on PlayPrime (rest bg).
pub const SETTLE_PLAY_INDEX_AWAIT_BATCH: u64 = 48;
/// Wall-clock wait for main decoder slot checkout (Chromium `setTimeout` clamp).
pub const SETTLE_TAKE_DECODER_WAIT_MS: f64 = 5_000.0;

/// Settle / PlayPrime timing budgets (local — not resolution/GOP branched yet).
///
/// Dual-decoder densify vs Exact needs brief yield windows and one-GOP byte warm;
/// mediabunny has **no equivalent** timers (`seekToTime` → `await videoFrameIterator?.return()`
/// then `startVideoIterator`). Soft-cold / poison paths stay named recovery helpers,
/// not fields here. Do **not** retune these when changing [`PlayPolicy`] Uhd or
/// [`ScrubPolicy`] MiB — different axes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SettlePolicy {
    /// First densify-busy drain after abort (ms).
    pub densify_yield_ms: f64,
    /// Second drain after re-abort (ms).
    pub densify_yield_retry_ms: f64,
    pub gop_sample_lead_min: u64,
    pub gop_sample_lead_max: u64,
    pub play_index_lead: u64,
    pub play_index_await_batch: u64,
    pub take_decoder_wait_ms: f64,
}

impl Default for SettlePolicy {
    fn default() -> Self {
        Self {
            densify_yield_ms: SETTLE_DENSIFY_YIELD_MS,
            densify_yield_retry_ms: SETTLE_DENSIFY_YIELD_RETRY_MS,
            gop_sample_lead_min: SETTLE_GOP_SAMPLE_LEAD_MIN,
            gop_sample_lead_max: SETTLE_GOP_SAMPLE_LEAD_MAX,
            play_index_lead: SETTLE_PLAY_INDEX_LEAD,
            play_index_await_batch: SETTLE_PLAY_INDEX_AWAIT_BATCH,
            take_decoder_wait_ms: SETTLE_TAKE_DECODER_WAIT_MS,
        }
    }
}

impl SettlePolicy {
    /// Clamp observed GOP length for Exact / PlayPrime sample warm.
    pub fn clamp_gop_sample_lead(self, gop_len: u64) -> u64 {
        gop_len.clamp(self.gop_sample_lead_min, self.gop_sample_lead_max)
    }
}

impl ResolutionClass {
    pub fn from_height(height: u32) -> Self {
        if height >= UHD_MIN_HEIGHT {
            Self::Uhd
        } else {
            Self::Hd
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{CodecConfig, VideoCodec, VideoMetadata};

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
    fn scrub_preview_prefers_closer_cache_over_key() {
        // Hole at 50; cache at 40; key at 0 → cache wins.
        assert_eq!(scrub_preview_choice(50, Some(0), Some(40)), Some(40));
        // Key at 48 closer than cache at 20 → key.
        assert_eq!(scrub_preview_choice(50, Some(48), Some(20)), Some(48));
        // Tie → cache (no decode flicker).
        assert_eq!(scrub_preview_choice(50, Some(40), Some(60)), Some(60));
        assert_eq!(scrub_preview_choice(50, Some(0), None), Some(0));
        assert_eq!(scrub_preview_choice(50, None, Some(44)), Some(44));
    }

    #[test]
    fn hover_thumb_dims_contain_into_box() {
        assert_eq!(hover_thumb_dims(1920, 1080, 160, 90), (160, 90));
        assert_eq!(hover_thumb_dims(1080, 1920, 160, 90), (51, 90));
        assert_eq!(hover_thumb_dims(100, 100, 160, 90), (90, 90));
    }

    #[test]
    fn next_missing_sync_key_nearest_unready() {
        let keys = [0u64, 48, 96, 144];
        assert_eq!(
            next_missing_sync_key(&keys, 100, |k| k == 96 || k == 48),
            Some(144)
        );
        assert_eq!(next_missing_sync_key(&keys, 50, |_| true), None);
        assert_eq!(next_missing_sync_key(&keys, 10, |_| false), Some(0));
    }

    #[test]
    fn nearer_gap_with_dir_distance_first() {
        // Closer behind wins even when scrubbing right (was: hard-prefer ahead).
        assert_eq!(nearer_gap_with_dir(150, Some(149), Some(500), 1), Some(149));
        assert_eq!(nearer_gap_with_dir(150, Some(0), Some(151), -1), Some(151));
        // Equal distance — dir tie-break; neutral → behind.
        assert_eq!(nearer_gap_with_dir(150, Some(99), Some(201), 1), Some(201));
        assert_eq!(nearer_gap_with_dir(150, Some(99), Some(201), -1), Some(99));
        assert_eq!(nearer_gap_with_dir(150, Some(99), Some(201), 0), Some(99));
        assert_eq!(nearer_gap_with_dir(120, Some(99), Some(201), 0), Some(99));
        assert_eq!(nearer_gap_with_dir(180, Some(99), Some(201), 0), Some(201));
        // One side only.
        assert_eq!(nearer_gap_with_dir(0, None, Some(8), -1), Some(8));
        assert_eq!(nearer_gap_with_dir(5, Some(3), None, 1), Some(3));
        let behind = 0u64.checked_sub(1);
        let ahead = 7u64.checked_add(1).filter(|&a| a <= 100);
        assert_eq!(nearer_gap_with_dir(0, behind, ahead, 0), Some(8));
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

    #[test]
    fn play_policy_hd_matches_historical_defaults() {
        let hd = PlayPolicy::from_resolution(1080);
        assert_eq!(hd, PlayPolicy::hd());
        assert_eq!(hd.presentation_cap, PLAY_PRESENTATION_CAP_HD);
        assert_eq!(hd.decode_warm_ahead, PLAY_WARM_AHEAD_HD);
        assert!(!hd.prefer_hardware);
        assert_eq!(hd.label(), "hd");
        // 1080p / 1440p stay Hd — do not retune working FHD.
        assert_eq!(PlayPolicy::from_resolution(1440).class, ResolutionClass::Hd);
    }

    #[test]
    fn play_policy_uhd_at_2160() {
        let uhd = PlayPolicy::from_resolution(2160);
        assert_eq!(uhd.class, ResolutionClass::Uhd);
        assert_eq!(uhd.presentation_cap, PLAY_PRESENTATION_CAP_UHD);
        assert_eq!(uhd.decode_warm_ahead, PLAY_WARM_AHEAD_UHD);
        assert!(uhd.prefer_hardware);
        assert_eq!(uhd.label(), "uhd");
        assert!(uhd.presentation_cap > PLAY_PRESENTATION_CAP_HD);
    }

    #[test]
    fn settle_policy_defaults_match_historical() {
        let s = SettlePolicy::default();
        assert_eq!(s.densify_yield_ms, SETTLE_DENSIFY_YIELD_MS);
        assert_eq!(s.densify_yield_retry_ms, SETTLE_DENSIFY_YIELD_RETRY_MS);
        assert_eq!(s.gop_sample_lead_min, SETTLE_GOP_SAMPLE_LEAD_MIN);
        assert_eq!(s.gop_sample_lead_max, SETTLE_GOP_SAMPLE_LEAD_MAX);
        assert_eq!(s.play_index_lead, SETTLE_PLAY_INDEX_LEAD);
        assert_eq!(s.play_index_await_batch, SETTLE_PLAY_INDEX_AWAIT_BATCH);
        assert_eq!(s.take_decoder_wait_ms, SETTLE_TAKE_DECODER_WAIT_MS);
        assert_eq!(s.clamp_gop_sample_lead(12), 12);
        assert_eq!(s.clamp_gop_sample_lead(4), SETTLE_GOP_SAMPLE_LEAD_MIN);
        assert_eq!(s.clamp_gop_sample_lead(100), SETTLE_GOP_SAMPLE_LEAD_MAX);
        // Clip-invariant for now — metadata always returns Default.
        let meta = VideoMetadata {
            width: 1920,
            height: 1080,
            timescale: 24_000,
            duration_ticks: 24_000,
            frame_count: 24,
            fps: Some(24.0),
            codec: CodecConfig {
                codec: VideoCodec::Avc,
                codec_string: None,
                description: vec![],
            },
            gop_profile: GopProfile::ShortFixed { len: 12 },
            timecode_start: None,
        };
        assert_eq!(meta.settle_policy(), SettlePolicy::default());
    }

    #[test]
    fn scrub_policy_from_profile() {
        let short = ScrubPolicy::from_gop_profile(GopProfile::ShortFixed { len: 30 });
        assert!(short.densify);
        assert_eq!(short.blit_mib, DEFAULT_SCRUB_BLIT_MIB_SHORT);
        assert_eq!(short.margin_left, EDGE_MARGIN_LEFT_MAX);
        assert_eq!(short.margin_right, EDGE_MARGIN_LEFT_MAX / 2);

        let short12 = ScrubPolicy::from_gop_profile(GopProfile::ShortFixed { len: 12 });
        assert_eq!(short12.margin_left, 12);
        assert_eq!(short12.margin_right, 6);

        let long = ScrubPolicy::from_gop_profile(GopProfile::Long {
            median: 120,
            max: 250,
        });
        assert!(long.densify);
        assert_eq!(long.blit_mib, DEFAULT_SCRUB_BLIT_MIB_LONG);
        assert_eq!(long.margin_left, EDGE_MARGIN_LEFT_MAX);
        assert!(long.margin_left > long.margin_right);

        let intra = ScrubPolicy::from_gop_profile(GopProfile::AllIntra);
        assert!(!intra.densify);
        assert_eq!(intra.blit_mib, DEFAULT_SCRUB_BLIT_MIB_SHORT);
        assert_eq!(intra.margin_left, EDGE_MARGIN_LEFT_MIN);
        assert_eq!(intra.margin_right, EDGE_MARGIN_RIGHT_MIN);
    }

    #[test]
    fn scrub_edge_margins_asymmetric_and_clamped() {
        assert_eq!(
            scrub_edge_margins(1),
            (EDGE_MARGIN_LEFT_MIN, EDGE_MARGIN_RIGHT_MIN)
        );
        assert_eq!(scrub_edge_margins(12), (12, 6));
        assert_eq!(
            scrub_edge_margins(250),
            (EDGE_MARGIN_LEFT_MAX, EDGE_MARGIN_LEFT_MAX / 2)
        );
        let (l, r) = scrub_edge_margins(30);
        assert!(l > r);
        assert!(l <= EDGE_MARGIN_LEFT_MAX);
        assert!(r >= EDGE_MARGIN_RIGHT_MIN);
    }

    #[test]
    fn scrub_gap_fill_armed_while_holes() {
        assert!(scrub_gap_fill_armed(false, 50, Some((40, 60)), 1000, 16, 8));
    }

    #[test]
    fn scrub_gap_fill_armed_near_edges_when_contiguous() {
        let extent = Some((100, 200));
        let last = 1000;
        assert!(!scrub_gap_fill_armed(true, 150, extent, last, 16, 16));
        assert!(scrub_gap_fill_armed(true, 110, extent, last, 16, 16));
        assert!(scrub_gap_fill_armed(true, 190, extent, last, 16, 16));
        assert!(scrub_gap_fill_armed(true, 50, extent, last, 16, 16));
        assert!(scrub_gap_fill_armed(true, 250, extent, last, 16, 16));
    }

    #[test]
    fn scrub_gap_fill_armed_asymmetric_left_wider() {
        let extent = Some((100, 200));
        let last = 1000;
        let left = 20;
        let right = 8;
        assert!(!scrub_gap_fill_armed(true, 150, extent, last, left, right));
        assert!(scrub_gap_fill_armed(true, 115, extent, last, left, right));
        assert!(!scrub_gap_fill_armed(true, 185, extent, last, left, right));
        assert!(scrub_gap_fill_armed(true, 195, extent, last, left, right));
    }

    #[test]
    fn scrub_gap_fill_armed_right_edge_slides_before_hard_edge() {
        // start==0: left margin must not arm (nowhere to expand behind).
        let extent = Some((0u64, 40u64));
        let last = 1000;
        assert!(scrub_gap_fill_armed(true, 38, extent, last, 8, 3));
        assert!(!scrub_gap_fill_armed(true, 20, extent, last, 8, 3));
        assert!(!scrub_gap_fill_armed(true, 5, extent, last, 8, 3));
    }

    #[test]
    fn scrub_gap_fill_armed_idle_at_pinned_timeline_edges() {
        // Playhead at origin with window pinned to 0 — was thrash (expand ahead,
        // evict far end, repeat).
        let at_start = Some((0u64, 50u64));
        assert!(!scrub_gap_fill_armed(true, 0, at_start, 200, 16, 8));
        assert!(!scrub_gap_fill_armed(true, 10, at_start, 200, 16, 8));
        // Still arm near the expandable right edge.
        assert!(scrub_gap_fill_armed(true, 45, at_start, 200, 16, 8));

        // Playhead at EOF with window pinned to last.
        let last = 200u64;
        let at_end = Some((150u64, last));
        assert!(!scrub_gap_fill_armed(true, last, at_end, last, 16, 8));
        assert!(!scrub_gap_fill_armed(true, 195, at_end, last, 16, 8));
        // Still arm near the expandable left edge.
        assert!(scrub_gap_fill_armed(true, 155, at_end, last, 16, 8));
    }

    #[test]
    fn scrub_blit_budget_scales_with_buffer() {
        // Short MiB ≈ SCRUB_BLIT_LRU frames at 1280×720 RGBA.
        assert_eq!(
            scrub_blit_frame_budget(1280, 720, DEFAULT_SCRUB_BLIT_MIB_SHORT),
            SCRUB_BLIT_LRU
        );
        // Larger buffer → fewer frames (same MiB).
        let at_1080 = scrub_blit_frame_budget(1920, 1080, DEFAULT_SCRUB_BLIT_MIB_SHORT);
        assert!(at_1080 < SCRUB_BLIT_LRU);
        assert!(at_1080 >= MIN_SCRUB_BLIT_FRAMES);
        // Long wallet > Short at same size.
        assert!(scrub_blit_frame_budget(1920, 1080, DEFAULT_SCRUB_BLIT_MIB_LONG) > at_1080);
        // Tiny CSS buffer clamps to MAX.
        assert_eq!(
            scrub_blit_frame_budget(160, 90, DEFAULT_SCRUB_BLIT_MIB_LONG),
            MAX_SCRUB_BLIT_FRAMES
        );
    }

    #[test]
    fn play_exact_vram_reserve_trims_but_keeps_floor() {
        // Same buffer as coded: YUV 1.5 vs RGBA 4 → ~3 VF ≈ 2 Exact slots (ceil).
        let reserve = exact_frames_to_free_for_play_vram(9, 1280, 720, 1280, 720);
        assert!(reserve >= 1);
        assert!(reserve < 9);
        let full = scrub_blit_frame_budget(1280, 720, DEFAULT_SCRUB_BLIT_MIB_SHORT);
        let during = scrub_blit_cap_during_play(full, reserve);
        assert!(during < full);
        assert!(during >= 1);
        assert_eq!(scrub_blit_cap_during_play(10, 100), 1);
        assert_eq!(
            exact_frames_to_free_for_play_vram(0, 1920, 1080, 1920, 1080),
            0
        );
    }
}
