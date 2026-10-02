//! Core media types. Frame index is primary; time is derived.

/// Zero-based presentation frame index.
pub type FrameId = u64;

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

/// Assumed scrubber CSS width for paint soft-merge (~1 px). Parity with
/// [`crate::source::fetch_map`] gray-strip scaling.
const STRIP_ASSUMED_SCRUBBER_PX: f64 = 800.0;

/// Missing frames between inclusive runs that still paint as &lt;1 px on an
/// ~800 px scrubber (`floor(max / 800)`; short clips → 0 = no soft-merge).
pub fn strip_merge_gap_frames(max_frame: f64) -> u64 {
    let max = max_frame.max(0.0);
    (max / STRIP_ASSUMED_SCRUBBER_PX).floor() as u64
}

/// Paint-only: merge inclusive `(lo, hi)` densify runs when the hole is ≤
/// `max_gap` frames. Engine / densify geometry stays exact via
/// [`coalesce_frame_runs`].
pub fn soft_merge_frame_runs(runs: &[(FrameId, FrameId)], max_gap: u64) -> Vec<(FrameId, FrameId)> {
    let mut out: Vec<(FrameId, FrameId)> = Vec::new();
    for &(start, end) in runs {
        let end = end.max(start);
        if let Some((_, prev_end)) = out.last_mut()
            && start <= prev_end.saturating_add(1).saturating_add(max_gap)
        {
            *prev_end = (*prev_end).max(end);
            continue;
        }
        out.push((start, end));
    }
    out
}

/// Hold a painted scrubber edge until rasterized coverage moves by more than
/// this many assumed scrubber pixels. Kills ±1 px soft-merge / densify /
/// FetchMap swim (“sometimes” blur). mediabunny: no equivalent meter.
pub const STRIP_PAINT_STICKY_PX: u16 = 2;

/// Inclusive-start / exclusive-end pixel runs on a scrubber of known width.
pub type StripPxRun = (u16, u16);

/// Sticky paint latch: keep last accepted runs until the candidate differs by
/// more than [`STRIP_PAINT_STICKY_PX`] rasterized pixels. Tracks buffer width
/// so a resize clears the hold.
#[derive(Debug, Clone, Default)]
pub struct StripPaintLatch {
    accepted: Vec<StripPxRun>,
    width: u16,
}

impl StripPaintLatch {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn resolve(&mut self, candidate: &[StripPxRun], width: u16) -> &[StripPxRun] {
        let width = width.max(1);
        if width != self.width {
            self.width = width;
            self.accepted = candidate.to_vec();
            return &self.accepted;
        }
        let next =
            stabilize_strip_segments(&self.accepted, candidate, STRIP_PAINT_STICKY_PX, width);
        if next.as_slice() != self.accepted.as_slice() {
            self.accepted = next;
        }
        &self.accepted
    }

    pub fn clear(&mut self) {
        self.accepted.clear();
        self.width = 0;
    }

    pub fn accepted(&self) -> &[StripPxRun] {
        &self.accepted
    }
}

/// Hamming distance of two run lists rasterized onto `width` pixels.
pub fn strip_raster_diff_px(a: &[StripPxRun], b: &[StripPxRun], width: u16) -> u32 {
    let mut diff = 0u32;
    for x in 0..width {
        if run_covers(a, x) != run_covers(b, x) {
            diff += 1;
        }
    }
    diff
}

fn run_covers(runs: &[StripPxRun], x: u16) -> bool {
    runs.iter().any(|&(lo, hi)| x >= lo && x < hi)
}

/// Paint hysteresis via raster diff: empty `next` clears; empty `prev` takes
/// `next`; coverage diff ≤ `sticky_px` keeps `prev`. Else take `next`.
pub fn stabilize_strip_segments(
    prev: &[StripPxRun],
    next: &[StripPxRun],
    sticky_px: u16,
    width: u16,
) -> Vec<StripPxRun> {
    if next.is_empty() {
        return Vec::new();
    }
    if prev.is_empty() {
        return next.to_vec();
    }
    if strip_raster_diff_px(prev, next, width.max(1)) <= u32::from(sticky_px) {
        return prev.to_vec();
    }
    next.to_vec()
}

/// Snap percentage segments (`0…100`) to integer pixel runs on `width_px`.
pub fn snap_pct_runs_to_width(
    segments: impl IntoIterator<Item = (f64, f64)>,
    width_px: u16,
) -> Vec<StripPxRun> {
    let width_px = width_px.max(1);
    let w = f64::from(width_px);
    let min_w = 1u16;
    let mut out: Vec<StripPxRun> = Vec::new();
    for (start, end) in segments {
        let start = start.clamp(0.0, 100.0);
        let end = end.clamp(0.0, 100.0);
        let mut lo = ((start / 100.0) * w).round() as i32;
        let mut hi = ((end / 100.0) * w).round() as i32;
        lo = lo.clamp(0, i32::from(width_px));
        hi = hi.clamp(0, i32::from(width_px));
        if hi < lo + i32::from(min_w) {
            hi = (lo + i32::from(min_w)).min(i32::from(width_px));
        }
        if hi <= lo {
            continue;
        }
        let lo = lo as u16;
        let hi = hi as u16;
        if let Some((_, prev_hi)) = out.last_mut()
            && lo <= prev_hi.saturating_add(1)
        {
            *prev_hi = (*prev_hi).max(hi);
            continue;
        }
        out.push((lo, hi));
    }
    out
}

/// Snap to the historical assumed ~800 px scrubber (tests / soft-merge parity).
pub fn snap_pct_runs_to_px(segments: impl IntoIterator<Item = (f64, f64)>) -> Vec<StripPxRun> {
    snap_pct_runs_to_width(segments, STRIP_ASSUMED_SCRUBBER_PX as u16)
}

/// File-byte fetch fractions (`0…1`) → snapped pixel runs on `width_px`.
pub fn fetch_strip_px_runs_for_width(ranges: &[(f32, f32)], width_px: u16) -> Vec<StripPxRun> {
    if ranges.is_empty() {
        return Vec::new();
    }
    snap_pct_runs_to_width(
        ranges
            .iter()
            .map(|&(a, b)| (a as f64 * 100.0, b as f64 * 100.0)),
        width_px,
    )
}

/// File-byte fetch fractions → snapped runs on the assumed 800 px scrubber.
pub fn fetch_strip_px_runs(ranges: &[(f32, f32)]) -> Vec<StripPxRun> {
    fetch_strip_px_runs_for_width(ranges, STRIP_ASSUMED_SCRUBBER_PX as u16)
}

/// Inclusive frame runs → snapped scrubber pixels (after optional soft-merge).
pub fn frame_runs_to_width(
    runs: &[(FrameId, FrameId)],
    max: f64,
    merge_gap: u64,
    width_px: u16,
) -> Vec<StripPxRun> {
    let max = max.max(1.0);
    let merged = soft_merge_frame_runs(runs, merge_gap);
    if merged.is_empty() {
        return Vec::new();
    }
    snap_pct_runs_to_width(
        merged.iter().map(|&(lo, hi)| {
            let start = (lo as f64 / max) * 100.0;
            let end = ((hi as f64 + 1.0) / max) * 100.0;
            (start, end)
        }),
        width_px,
    )
}

/// Inclusive frame runs → snapped pixels on the assumed 800 px scrubber.
pub fn frame_runs_to_px(runs: &[(FrameId, FrameId)], max: f64, merge_gap: u64) -> Vec<StripPxRun> {
    frame_runs_to_width(runs, max, merge_gap, STRIP_ASSUMED_SCRUBBER_PX as u16)
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
/// - `dir > 0` — hard prefer `ahead` (scrub right); fallback `behind`.
/// - `dir < 0` — hard prefer `behind` (scrub left); fallback `ahead`.
/// - `dir == 0` — min `|gap − playhead|`; tie → behind (PREV-friendly neutral).
///
/// Local invention (predictive densify); mediabunny has no equivalent.
pub fn nearer_gap_with_dir(
    playhead: FrameId,
    behind: Option<FrameId>,
    ahead: Option<FrameId>,
    dir: i8,
) -> Option<FrameId> {
    if dir > 0 {
        return ahead.or(behind);
    }
    if dir < 0 {
        return behind.or(ahead);
    }
    match (behind, ahead) {
        (Some(b), Some(a)) => {
            if playhead.abs_diff(b) <= playhead.abs_diff(a) {
                Some(b)
            } else {
                Some(a)
            }
        }
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

/// Drag paint strategy during timeline scrub (`scrub_to(settle=false)`).
///
/// Local invention (mediabunny has no equivalent). Settle is always exact VF.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScrubDragMode {
    /// Blit / neighbor preview only — no key→N during drag (short-IP / All-Intra).
    Soft,
    /// May decode exact during drag (Long GOP exact-primary).
    Exact,
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

/// Per-clip scrub knobs derived from [`GopProfile`] (not global constants).
///
/// Local invention (mediabunny: no equivalent). Tune fields per profile; do not
/// raise globals like [`SCRUB_BLIT_LRU`] from a single ShotGrid probe.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScrubPolicy {
    pub drag: ScrubDragMode,
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
                drag: ScrubDragMode::Soft,
                densify: false,
                blit_mib: DEFAULT_SCRUB_BLIT_MIB_SHORT,
                scrub_cache_mib: DEFAULT_SCRUB_CACHE_MIB,
                margin_left,
                margin_right,
            },
            GopProfile::ShortFixed { .. } => Self {
                drag: ScrubDragMode::Soft,
                densify: true,
                blit_mib: DEFAULT_SCRUB_BLIT_MIB_SHORT,
                scrub_cache_mib: DEFAULT_SCRUB_CACHE_MIB,
                margin_left,
                margin_right,
            },
            GopProfile::Long { .. } => Self {
                drag: ScrubDragMode::Exact,
                densify: true,
                blit_mib: DEFAULT_SCRUB_BLIT_MIB_LONG,
                scrub_cache_mib: DEFAULT_SCRUB_CACHE_MIB,
                margin_left,
                margin_right,
            },
        }
    }

    pub fn soft_drag(self) -> bool {
        matches!(self.drag, ScrubDragMode::Soft)
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
/// Loop prime buffer — Hd (historical default 8).
pub const PLAY_LOOP_PRIME_FRAMES_HD: usize = 8;
/// Loop prime buffer — Uhd (≥ presentation_cap so restart is not starved).
pub const PLAY_LOOP_PRIME_FRAMES_UHD: usize = 12;
/// Near-EOF / loop-prepare wall budget — Hd.
pub const PLAY_EOF_BUDGET_SECS_HD: f64 = 0.4;
/// Near-EOF / loop-prepare wall budget — Uhd (4K last-GOP decode is slower).
pub const PLAY_EOF_BUDGET_SECS_UHD: f64 = 1.0;
/// Extra near-EOF lead for non-loop Uhd (0 = Hd: no special non-loop EOF zone).
pub const PLAY_EOF_LEAD_SECS_HD: f64 = 0.0;
pub const PLAY_EOF_LEAD_SECS_UHD: f64 = 1.0;

/// Per-clip **play** knobs (primary). Derived from coded height, not GOP.
///
/// Local invention for Uhd queue/warm/EOF (mediabunny has no resolution play
/// profile). `prefer_hardware` on the policy documents Uhd intent; **main** play
/// always probes HW when allowed (`WebCodecsDecoder::create`). Scrub/thumb keep
/// [`PlayPolicy::hd`] with no HW (mediabunny `decoderOptions.hardwareAcceleration`,
/// PR #406 — dual HW stalls).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PlayPolicy {
    pub class: ResolutionClass,
    /// Presentation-queue ahead while playing (non-shuttle).
    pub presentation_cap: usize,
    /// Packet-pump warm frames past the consumer target.
    pub decode_warm_ahead: usize,
    /// Uhd documents HW intent; scrub densify uses [`Self::hd`] (`false`) and
    /// never steals HW. Main play probes HW via `create` regardless of Hd/Uhd.
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

    /// Frames to decode into `loop_prime` before the seam.
    pub fn loop_prime_frames(self) -> usize {
        match self.class {
            ResolutionClass::Hd => PLAY_LOOP_PRIME_FRAMES_HD,
            ResolutionClass::Uhd => PLAY_LOOP_PRIME_FRAMES_UHD,
        }
    }

    /// Wall seconds of presentation to keep near EOF while loop-prime fills
    /// (also sizes the loop near-EOF cap).
    pub fn loop_prime_budget_secs(self) -> f64 {
        match self.class {
            ResolutionClass::Hd => PLAY_EOF_BUDGET_SECS_HD,
            ResolutionClass::Uhd => PLAY_EOF_BUDGET_SECS_UHD,
        }
    }

    /// Non-loop near-EOF lead: pull the remaining tail into the presentation
    /// queue early so 4K last-GOP decode does not underrun before `Ended`.
    /// Hd is `0` — FHD path unchanged.
    pub fn eof_lead_secs(self) -> f64 {
        match self.class {
            ResolutionClass::Hd => PLAY_EOF_LEAD_SECS_HD,
            ResolutionClass::Uhd => PLAY_EOF_LEAD_SECS_UHD,
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
/// Presentation frames seeded before clock start / Exact→Play skip-fill.
pub const SETTLE_PLAY_PRIME_FRAMES: usize = 2;
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
    pub play_prime_frames: usize,
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
            play_prime_frames: SETTLE_PLAY_PRIME_FRAMES,
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

    #[test]
    fn strip_merge_gap_scales_with_timeline() {
        assert_eq!(strip_merge_gap_frames(100.0), 0);
        assert_eq!(strip_merge_gap_frames(800.0), 1);
        assert_eq!(strip_merge_gap_frames(14_400.0), 18); // BBB-class ~10 min @24
    }

    #[test]
    fn soft_merge_frame_runs_closes_subpixel_holes() {
        let runs = [(0u64, 10), (15, 30), (100, 110)];
        assert_eq!(
            soft_merge_frame_runs(&runs, 0),
            vec![(0, 10), (15, 30), (100, 110)]
        );
        assert_eq!(
            soft_merge_frame_runs(&runs, 4),
            vec![(0, 30), (100, 110)],
            "4-frame hole (11..=14) merges; 69-frame hole stays"
        );
        // Long-clip paint path: gap of 10 < floor(14400/800)=18.
        let gap = strip_merge_gap_frames(14_400.0);
        assert_eq!(
            soft_merge_frame_runs(&[(0, 100), (111, 200)], gap),
            vec![(0, 200)]
        );
        assert_eq!(
            soft_merge_frame_runs(&[(0, 100), (200, 300)], gap).len(),
            2,
            "visible multi-px hole stays split"
        );
    }

    #[test]
    fn stabilize_holds_sub_sticky_oscillation() {
        let w = 800u16;
        let prev = vec![(0, 100)];
        assert_eq!(
            stabilize_strip_segments(&prev, &[(0, 101)], STRIP_PAINT_STICKY_PX, w),
            prev
        );
        assert_eq!(
            stabilize_strip_segments(&prev, &[(1, 100)], STRIP_PAINT_STICKY_PX, w),
            prev
        );
        assert_eq!(
            stabilize_strip_segments(&prev, &[(0, 99)], STRIP_PAINT_STICKY_PX, w),
            prev
        );
    }

    #[test]
    fn stabilize_accepts_real_growth_past_sticky() {
        let prev = vec![(0, 100)];
        let next = vec![(0, 103)];
        assert_eq!(
            stabilize_strip_segments(&prev, &next, STRIP_PAINT_STICKY_PX, 800),
            next
        );
    }

    #[test]
    fn stabilize_accepts_clear_and_first_paint() {
        assert!(stabilize_strip_segments(&[(0, 10)], &[], 2, 800).is_empty());
        assert_eq!(
            stabilize_strip_segments(&[], &[(0, 10)], 2, 800),
            vec![(0, 10)]
        );
    }

    #[test]
    fn latch_kills_raf_edge_thrash() {
        let w = 800u16;
        let mut latch = StripPaintLatch::new();
        assert_eq!(latch.resolve(&[(0, 200)], w).to_vec(), vec![(0, 200)]);
        let mut unique = std::collections::BTreeSet::new();
        unique.insert(format!("{:?}", latch.accepted()));
        for i in 0..60 {
            let end = if i % 2 == 0 { 201 } else { 199 };
            let runs = latch.resolve(&[(0, end)], w).to_vec();
            unique.insert(format!("{runs:?}"));
        }
        assert_eq!(unique.len(), 1, "sub-sticky thrash must not rewrite runs");
        let grown = latch.resolve(&[(0, 220)], w).to_vec();
        assert_eq!(grown, vec![(0, 220)]);
        unique.insert(format!("{grown:?}"));
        assert_eq!(unique.len(), 2);
    }

    #[test]
    fn latch_fetch_fraction_jitter() {
        let w = 800u16;
        let mut latch = StripPaintLatch::new();
        let a = fetch_strip_px_runs_for_width(&[(0.0, 0.5)], w);
        let painted = latch.resolve(&a, w).to_vec();
        let mut unique = std::collections::BTreeSet::new();
        unique.insert(format!("{painted:?}"));
        for i in 0..30 {
            let alt = if i % 2 == 0 {
                fetch_strip_px_runs_for_width(&[(0.0, 0.50125)], w)
            } else {
                fetch_strip_px_runs_for_width(&[(0.0, 0.5)], w)
            };
            unique.insert(format!("{:?}", latch.resolve(&alt, w)));
        }
        assert_eq!(unique.len(), 1, "fetch f32 jitter: {unique:?}");
    }

    #[test]
    fn covered_soft_merge_flip_held_by_raster_latch() {
        let w = 800u16;
        let merge_gap = strip_merge_gap_frames(8000.0).saturating_mul(2);
        let a = frame_runs_to_width(&[(0, 0), (15, 15)], 8000.0, merge_gap, w);
        let b = frame_runs_to_width(&[(0, 0), (25, 25)], 8000.0, merge_gap, w);
        assert!(
            strip_raster_diff_px(&a, &b, w) > 0,
            "merged vs split must differ before latch"
        );
        let mut latch = StripPaintLatch::new();
        let _ = latch.resolve(&a, w);
        let mut unique = std::collections::BTreeSet::new();
        unique.insert(format!("{:?}", latch.accepted()));
        for i in 0..40 {
            let cand = if i % 2 == 0 { &a } else { &b };
            unique.insert(format!("{:?}", latch.resolve(cand, w)));
        }
        assert_eq!(
            unique.len(),
            1,
            "Covered merge↔split near 1–2 px must hold: diff={}",
            strip_raster_diff_px(&a, &b, w)
        );
    }

    #[test]
    fn many_holes_snap_to_integer_device_pixels() {
        // Long-clip FetchMap: 200 islands → canvas path uses integer rects on
        // real buffer width (not a 800-stop CSS gradient).
        let w = 1280u16;
        let mut fracs = Vec::new();
        for i in 0..200 {
            let a = i as f32 / 400.0;
            let b = a + 0.001;
            fracs.push((a, b.min(1.0)));
        }
        let runs = fetch_strip_px_runs_for_width(&fracs, w);
        assert!(!runs.is_empty());
        for &(lo, hi) in &runs {
            assert!(lo < hi);
            assert!(hi <= w);
        }
        // Abutting after snap may coalesce — still ≪ raw island×4 CSS stops.
        assert!(runs.len() <= 200);
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
    fn nearer_gap_with_dir_prefers_along_scrub() {
        assert_eq!(nearer_gap_with_dir(150, Some(99), Some(201), 1), Some(201));
        assert_eq!(nearer_gap_with_dir(150, Some(99), Some(201), -1), Some(99));
        // Equal distance — neutral still prefers behind.
        assert_eq!(nearer_gap_with_dir(150, Some(99), Some(201), 0), Some(99));
        assert_eq!(nearer_gap_with_dir(120, Some(99), Some(201), 0), Some(99));
        assert_eq!(nearer_gap_with_dir(180, Some(99), Some(201), 0), Some(201));
        // Missing side along dir → fallback.
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
        assert_eq!(uhd.loop_prime_frames(), PLAY_LOOP_PRIME_FRAMES_UHD);
        assert!(uhd.loop_prime_budget_secs() > PlayPolicy::hd().loop_prime_budget_secs());
        assert!(uhd.eof_lead_secs() > 0.0);
        assert_eq!(PlayPolicy::hd().eof_lead_secs(), 0.0);
        assert_eq!(
            PlayPolicy::hd().loop_prime_frames(),
            PLAY_LOOP_PRIME_FRAMES_HD
        );
    }

    #[test]
    fn settle_policy_defaults_match_historical() {
        let s = SettlePolicy::default();
        assert_eq!(s.densify_yield_ms, SETTLE_DENSIFY_YIELD_MS);
        assert_eq!(s.densify_yield_retry_ms, SETTLE_DENSIFY_YIELD_RETRY_MS);
        assert_eq!(s.gop_sample_lead_min, SETTLE_GOP_SAMPLE_LEAD_MIN);
        assert_eq!(s.gop_sample_lead_max, SETTLE_GOP_SAMPLE_LEAD_MAX);
        assert_eq!(s.play_prime_frames, SETTLE_PLAY_PRIME_FRAMES);
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
        assert!(short.soft_drag());
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
        assert_eq!(long.drag, ScrubDragMode::Exact);
        assert!(long.densify);
        assert_eq!(long.blit_mib, DEFAULT_SCRUB_BLIT_MIB_LONG);
        assert_eq!(long.margin_left, EDGE_MARGIN_LEFT_MAX);
        assert!(long.margin_left > long.margin_right);

        let intra = ScrubPolicy::from_gop_profile(GopProfile::AllIntra);
        assert!(intra.soft_drag());
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
    Other,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AudioCodecConfig {
    pub codec: AudioCodec,
    /// WebCodecs `AudioDecoderConfig.codec` (e.g. `mp4a.40.2`).
    pub codec_string: Option<String>,
    /// AudioSpecificConfig bytes for `description` (rebuilt from esds when needed).
    pub description: Vec<u8>,
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
