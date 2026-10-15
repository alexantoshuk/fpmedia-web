//! Scrubber strip paint math: frame runs / fetch fractions → continuous runs.
//!
//! Hosts paint with a 1 px-tall canvas + float `fillRect` on the same `f/n·w`
//! grid as tip / zebra (Chrome blurs many-stop CSS gradients on long clips).
//! mediabunny: no meter.

#[cfg(target_arch = "wasm32")]
use crate::video::{ReadyFrameKind, ReadyFrameMark};
use fpmedia_web::FrameId;
#[cfg(target_arch = "wasm32")]
use wasm_bindgen::JsCast;

/// Dim Covered solid ≈ `#34d399` @ 0.35 over `#141414` (ready track).
pub const COVERED_STRIP_COLOR: &str = "#1f5742";

/// Bright Exact (full `#34d399`).
pub const EXACT_STRIP_COLOR: &str = "#34d399";

/// Gray FetchMap solid.
pub const FETCH_STRIP_COLOR: &str = "#6b7280";

/// Yellow keyframe / sync ticks on the ready strip.
pub const KEYFRAME_STRIP_COLOR: &str = "#eab308";

/// Frame marks → continuous runs on `width` (same `f/n` as tip / zebra).
/// Covered/Exact: honest (no soft-merge / no min-px inflate).
/// Keyframes: `min_run_px = 1` so sync ticks stay visible on long clips.
#[cfg(target_arch = "wasm32")]
pub fn cache_strip_runs_for_width(
    marks: &[ReadyFrameMark],
    kind: ReadyFrameKind,
    frame_count: f64,
    width: f64,
) -> Vec<StripRun> {
    let mut runs: Vec<(FrameId, FrameId)> = marks
        .iter()
        .filter(|m| m.kind == kind)
        .map(|m| (m.frame, m.end.max(m.frame)))
        .collect();
    runs.sort_by_key(|&(lo, _)| lo);
    match kind {
        ReadyFrameKind::Keyframes => keyframe_runs_to_width(&runs, frame_count, width),
        ReadyFrameKind::Covered | ReadyFrameKind::Exact => {
            ready_runs_to_width(&runs, frame_count, width)
        }
    }
}

/// Sorted inclusive ready runs → continuous runs on `width`.
/// View = data feedback: holes stay; no min-px inflate (hairlines OK).
pub fn ready_runs_to_width(
    runs: &[(FrameId, FrameId)],
    frame_count: f64,
    width: f64,
) -> Vec<StripRun> {
    frame_runs_to_width(runs, frame_count.max(1.0), 0, width)
}

/// Sync / keyframe ticks → continuous runs; inflate to ≥1 px.
pub fn keyframe_runs_to_width(
    runs: &[(FrameId, FrameId)],
    frame_count: f64,
    width: f64,
) -> Vec<StripRun> {
    let n = frame_count.max(1.0);
    if runs.is_empty() {
        return Vec::new();
    }
    snap_pct_runs_to_width(
        runs.iter().map(|&(lo, hi)| {
            let start = (lo as f64 / n) * 100.0;
            let end = ((hi as f64 + 1.0) / n) * 100.0;
            (start, end)
        }),
        width,
        1.0,
    )
}

/// Sorted inclusive ready runs → snapped integer pixel runs on `width_px`.
/// Kept for sticky/unit probes; paint path uses [`ready_runs_to_width`].
pub fn ready_runs_to_width_px(
    runs: &[(FrameId, FrameId)],
    frame_count: f64,
    width_px: u16,
) -> Vec<StripPxRun> {
    frame_runs_to_width_px(runs, frame_count.max(1.0), 0, width_px)
}

/// Sync / keyframe ticks → snapped integer runs; inflate to ≥1 device px.
pub fn keyframe_runs_to_width_px(
    runs: &[(FrameId, FrameId)],
    frame_count: f64,
    width_px: u16,
) -> Vec<StripPxRun> {
    let n = frame_count.max(1.0);
    if runs.is_empty() {
        return Vec::new();
    }
    snap_pct_runs_to_width_px(
        runs.iter().map(|&(lo, hi)| {
            let start = (lo as f64 / n) * 100.0;
            let end = ((hi as f64 + 1.0) / n) * 100.0;
            (start, end)
        }),
        width_px,
        1,
    )
}

/// Device-pixel buffer width for a 1 px-tall strip canvas (`css_w × dpr`).
pub fn strip_canvas_buffer_width(css_width: f64, device_pixel_ratio: f64) -> u16 {
    let dpr = device_pixel_ratio.max(1.0);
    let w = (css_width.max(0.0) * dpr).round();
    (w as u16).max(1)
}

/// Paint opaque runs into a **1 px-tall** canvas with float `fillRect`.
/// Stretch the canvas in CSS to the strip height. `runs` in buffer space.
#[cfg(target_arch = "wasm32")]
pub fn paint_strip_canvas(
    canvas: &web_sys::HtmlCanvasElement,
    buffer_width: u16,
    runs: &[StripRun],
    color: &str,
) -> bool {
    let w = u32::from(buffer_width.max(1));
    if canvas.width() != w || canvas.height() != 1 {
        canvas.set_width(w);
        canvas.set_height(1);
    }
    let Ok(Some(ctx_obj)) = canvas.get_context("2d") else {
        return false;
    };
    let Ok(ctx) = ctx_obj.dyn_into::<web_sys::CanvasRenderingContext2d>() else {
        return false;
    };
    let wf = f64::from(w);
    ctx.clear_rect(0.0, 0.0, wf, 1.0);
    ctx.set_fill_style_str(color);
    for &(lo, hi) in runs {
        let span = hi - lo;
        if span > 0.0 {
            ctx.fill_rect(lo, 0.0, span, 1.0);
        }
    }
    true
}

/// Assumed scrubber CSS width for paint soft-merge (~1 px). Parity with
/// `fpmedia_web` `source::fetch_map` gray-strip scaling.
const STRIP_ASSUMED_SCRUBBER_PX: f64 = 800.0;

/// Missing frames between inclusive runs that still paint as &lt;1 px on an
/// ~800 px scrubber (`floor(max / 800)`; short clips → 0 = no soft-merge).
pub fn strip_merge_gap_frames(max_frame: f64) -> u64 {
    let max = max_frame.max(0.0);
    (max / STRIP_ASSUMED_SCRUBBER_PX).floor() as u64
}

/// Paint-only: merge inclusive `(lo, hi)` densify runs when the hole is ≤
/// `max_gap` frames. Engine / densify geometry stays exact via
/// [`fpmedia_web::coalesce_frame_runs`].
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
/// this many assumed scrubber pixels. Kills ±1 px densify / FetchMap swim
/// (“sometimes” blur). mediabunny: no equivalent meter.
pub const STRIP_PAINT_STICKY_PX: u16 = 2;

/// Inclusive-start / exclusive-end integer pixel runs (sticky / unit probes).
pub type StripPxRun = (u16, u16);

/// Continuous half-open runs on buffer width (paint path / tip / zebra grid).
pub type StripRun = (f64, f64);

/// Sticky paint latch for continuous runs: sample coverage on integer columns.
#[derive(Debug, Clone, Default)]
pub struct StripPaintLatch {
    accepted: Vec<StripRun>,
    width: u16,
}

impl StripPaintLatch {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn resolve(&mut self, candidate: &[StripRun], width: u16) -> &[StripRun] {
        let width = width.max(1);
        if width != self.width {
            self.width = width;
            self.accepted = candidate.to_vec();
            return &self.accepted;
        }
        let next =
            stabilize_strip_segments(&self.accepted, candidate, STRIP_PAINT_STICKY_PX, width);
        if !strip_runs_eq(&next, &self.accepted) {
            self.accepted = next;
        }
        &self.accepted
    }

    pub fn clear(&mut self) {
        self.accepted.clear();
        self.width = 0;
    }

    pub fn accepted(&self) -> &[StripRun] {
        &self.accepted
    }
}

fn strip_runs_eq(a: &[StripRun], b: &[StripRun]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter()
        .zip(b.iter())
        .all(|(&(lo0, hi0), &(lo1, hi1))| (lo0 - lo1).abs() < 1e-9 && (hi0 - hi1).abs() < 1e-9)
}

/// Hamming distance of two integer run lists rasterized onto `width` pixels.
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

fn run_covers_mid(runs: &[StripRun], x_mid: f64) -> bool {
    runs.iter().any(|&(lo, hi)| x_mid >= lo && x_mid < hi)
}

/// Sample continuous runs on integer column centers; count coverage mismatches.
pub fn strip_raster_diff(a: &[StripRun], b: &[StripRun], width: u16) -> u32 {
    let mut diff = 0u32;
    for x in 0..width {
        let mid = f64::from(x) + 0.5;
        if run_covers_mid(a, mid) != run_covers_mid(b, mid) {
            diff += 1;
        }
    }
    diff
}

/// Paint hysteresis via raster diff (integer runs — unit probes).
pub fn stabilize_strip_segments_px(
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

/// Paint hysteresis for continuous runs (sample integer column centers).
pub fn stabilize_strip_segments(
    prev: &[StripRun],
    next: &[StripRun],
    sticky_px: u16,
    width: u16,
) -> Vec<StripRun> {
    if next.is_empty() {
        return Vec::new();
    }
    if prev.is_empty() {
        return next.to_vec();
    }
    if strip_raster_diff(prev, next, width.max(1)) <= u32::from(sticky_px) {
        return prev.to_vec();
    }
    next.to_vec()
}

/// Continuous percentage segments (`0…100`) → float runs on `width`.
///
/// `min_run_px == 0`: drop empty spans. `min_run_px > 0`: inflate short runs
/// (Fetch / Keyframes — keep visible ticks).
pub fn snap_pct_runs_to_width(
    segments: impl IntoIterator<Item = (f64, f64)>,
    width: f64,
    min_run_px: f64,
) -> Vec<StripRun> {
    let w = width.max(1.0);
    let mut out: Vec<StripRun> = Vec::new();
    for (start, end) in segments {
        let start = start.clamp(0.0, 100.0);
        let end = end.clamp(0.0, 100.0);
        let mut lo = (start / 100.0) * w;
        let mut hi = (end / 100.0) * w;
        lo = lo.clamp(0.0, w);
        hi = hi.clamp(0.0, w);
        if min_run_px > 0.0 && hi < lo + min_run_px {
            hi = (lo + min_run_px).min(w);
        }
        if hi <= lo {
            continue;
        }
        if let Some((_, prev_hi)) = out.last_mut()
            && lo <= *prev_hi
        {
            *prev_hi = (*prev_hi).max(hi);
            continue;
        }
        out.push((lo, hi));
    }
    out
}

/// Snap percentage segments (`0…100`) to integer pixel runs on `width_px`.
///
/// `min_run_px == 0`: drop runs that round to empty (ready greens — honest).
/// `min_run_px >= 1`: inflate short runs (gray FetchMap — keep visible ticks).
pub fn snap_pct_runs_to_width_px(
    segments: impl IntoIterator<Item = (f64, f64)>,
    width_px: u16,
    min_run_px: u16,
) -> Vec<StripPxRun> {
    let width_px = width_px.max(1);
    let w = f64::from(width_px);
    let mut out: Vec<StripPxRun> = Vec::new();
    for (start, end) in segments {
        let start = start.clamp(0.0, 100.0);
        let end = end.clamp(0.0, 100.0);
        let mut lo = ((start / 100.0) * w).round() as i32;
        let mut hi = ((end / 100.0) * w).round() as i32;
        lo = lo.clamp(0, i32::from(width_px));
        hi = hi.clamp(0, i32::from(width_px));
        if min_run_px > 0 && hi < lo + i32::from(min_run_px) {
            hi = (lo + i32::from(min_run_px)).min(i32::from(width_px));
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
    snap_pct_runs_to_width_px(segments, STRIP_ASSUMED_SCRUBBER_PX as u16, 1)
}

/// File-byte fetch fractions (`0…1`) → continuous runs on `width` (min 1 px).
pub fn fetch_strip_runs_for_width(ranges: &[(f32, f32)], width: f64) -> Vec<StripRun> {
    if ranges.is_empty() {
        return Vec::new();
    }
    snap_pct_runs_to_width(
        ranges
            .iter()
            .map(|&(a, b)| (a as f64 * 100.0, b as f64 * 100.0)),
        width,
        1.0,
    )
}

/// File-byte fetch fractions (`0…1`) → snapped pixel runs on `width_px`.
pub fn fetch_strip_px_runs_for_width(ranges: &[(f32, f32)], width_px: u16) -> Vec<StripPxRun> {
    if ranges.is_empty() {
        return Vec::new();
    }
    snap_pct_runs_to_width_px(
        ranges
            .iter()
            .map(|&(a, b)| (a as f64 * 100.0, b as f64 * 100.0)),
        width_px,
        1,
    )
}

/// File-byte fetch fractions → snapped runs on the assumed 800 px scrubber.
pub fn fetch_strip_px_runs(ranges: &[(f32, f32)]) -> Vec<StripPxRun> {
    fetch_strip_px_runs_for_width(ranges, STRIP_ASSUMED_SCRUBBER_PX as u16)
}

/// Inclusive frame runs → continuous scrubber spans (after optional soft-merge).
/// Ready greens: `merge_gap = 0` + `min_run_px = 0` (view mirrors data).
pub fn frame_runs_to_width(
    runs: &[(FrameId, FrameId)],
    frame_count: f64,
    merge_gap: u64,
    width: f64,
) -> Vec<StripRun> {
    let n = frame_count.max(1.0);
    let merged = soft_merge_frame_runs(runs, merge_gap);
    if merged.is_empty() {
        return Vec::new();
    }
    snap_pct_runs_to_width(
        merged.iter().map(|&(lo, hi)| {
            let start = (lo as f64 / n) * 100.0;
            let end = ((hi as f64 + 1.0) / n) * 100.0;
            (start, end)
        }),
        width,
        0.0,
    )
}

/// Inclusive frame runs → snapped scrubber pixels (after optional soft-merge).
pub fn frame_runs_to_width_px(
    runs: &[(FrameId, FrameId)],
    frame_count: f64,
    merge_gap: u64,
    width_px: u16,
) -> Vec<StripPxRun> {
    let n = frame_count.max(1.0);
    let merged = soft_merge_frame_runs(runs, merge_gap);
    if merged.is_empty() {
        return Vec::new();
    }
    snap_pct_runs_to_width_px(
        merged.iter().map(|&(lo, hi)| {
            let start = (lo as f64 / n) * 100.0;
            let end = ((hi as f64 + 1.0) / n) * 100.0;
            (start, end)
        }),
        width_px,
        0,
    )
}

/// Inclusive frame runs → snapped pixels on the assumed 800 px scrubber.
pub fn frame_runs_to_px(
    runs: &[(FrameId, FrameId)],
    frame_count: f64,
    merge_gap: u64,
) -> Vec<StripPxRun> {
    frame_runs_to_width_px(
        runs,
        frame_count,
        merge_gap,
        STRIP_ASSUMED_SCRUBBER_PX as u16,
    )
}

/// Half-open pixel span for presentation frame `frame` on a `frame_count` grid.
///
/// Integer snap for strip paint:  
/// `round(f/n·w) .. round((f+1)/n·w)` clamped into `[0, width_px]`.
pub fn frame_slot_px(frame: u32, frame_count: u32, width_px: u32) -> (u32, u32) {
    if frame_count == 0 || width_px == 0 {
        return (0, 0);
    }
    let n = frame_count;
    let f = frame.min(n - 1);
    let nf = f64::from(n);
    let wf = f64::from(width_px);
    let lo = ((f as f64 / nf) * wf).round().clamp(0.0, wf) as u32;
    let hi = (((f as f64 + 1.0) / nf) * wf).round().clamp(0.0, wf) as u32;
    (lo, hi)
}

/// Half-open span matching [`scrub_frame_from_x`]: column `x` → `floor(x/w·n)`.
///
/// `lo = ceil(f·w/n)`, `hi = ceil((f+1)·w/n)` (integer). Kept for strip probes;
/// tip / zebra demo path uses [`frame_slot`].
pub fn frame_slot_floor_px(frame: u32, frame_count: u32, width_px: u32) -> (u32, u32) {
    if frame_count == 0 || width_px == 0 {
        return (0, 0);
    }
    let n = u64::from(frame_count);
    let w = u64::from(width_px);
    let f = u64::from(frame.min(frame_count - 1));
    let lo = (f * w).div_ceil(n).min(w) as u32;
    let hi = ((f + 1) * w).div_ceil(n).min(w) as u32;
    (lo, hi.max(lo))
}

/// Continuous half-open slot: `[f/n·w, (f+1)/n·w)` — no integer snap.
pub fn frame_slot(frame: u32, frame_count: u32, width: f64) -> (f64, f64) {
    if frame_count == 0 || width <= 0.0 {
        return (0.0, 0.0);
    }
    let n = f64::from(frame_count);
    let f = f64::from(frame.min(frame_count - 1));
    ((f / n) * width, ((f + 1.0) / n) * width)
}

/// Equal continuous step `w/n` (0 when empty / non-positive width).
pub fn frame_zebra_step(frame_count: u32, width: f64) -> f64 {
    if frame_count == 0 || width <= 0.0 {
        return 0.0;
    }
    width / f64::from(frame_count)
}

/// Integer floor slot width `floor(width_px / frame_count)` (probe / tests).
pub fn frame_zebra_slot_px(frame_count: u32, width_px: u32) -> u32 {
    if frame_count == 0 || width_px == 0 {
        return 0;
    }
    width_px / frame_count
}

/// Demo zebra min slot (CSS px): show when continuous step `w/n` is at least this.
pub const FRAME_ZEBRA_MIN_SLOT_PX: f64 = 1.5;

/// True when continuous step `w/n ≥ [`FRAME_ZEBRA_MIN_SLOT_PX`]`.
pub fn frame_slots_at_least_min(frame_count: u32, width_px: u16) -> bool {
    frame_zebra_step(frame_count, f64::from(width_px)) >= FRAME_ZEBRA_MIN_SLOT_PX
}

/// Odd-frame runs as continuous `[lo, hi)` on width (canvas `fillRect` floats).
/// Empty when step `w/n < [`FRAME_ZEBRA_MIN_SLOT_PX`].
pub fn odd_frame_runs(frame_count: u32, width: f64) -> Vec<(f64, f64)> {
    if frame_zebra_step(frame_count, width) < FRAME_ZEBRA_MIN_SLOT_PX {
        return Vec::new();
    }
    let mut out = Vec::new();
    for f in (1..frame_count).step_by(2) {
        let (lo, hi) = frame_slot(f, frame_count, width);
        if hi <= lo {
            continue;
        }
        out.push((lo, hi));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_strips_are_empty_runs() {
        assert!(ready_runs_to_width_px(&[], 10.0, 800).is_empty());
        assert!(fetch_strip_px_runs(&[]).is_empty());
    }

    #[test]
    fn fetch_runs_snap_to_pixel_grid() {
        let runs = fetch_strip_px_runs(&[(0.0, 0.5)]);
        assert_eq!(runs, vec![(0, 400)]);
    }

    #[test]
    fn strip_buffer_width_scales_dpr() {
        assert_eq!(strip_canvas_buffer_width(800.0, 1.0), 800);
        assert_eq!(strip_canvas_buffer_width(800.0, 2.0), 1600);
        assert_eq!(strip_canvas_buffer_width(0.0, 2.0), 1);
    }

    #[test]
    fn ready_greens_keep_holes_no_soft_merge() {
        let far = [(0, 0), (100, 100)];
        let exact = ready_runs_to_width_px(&far, 8000.0, 800);
        let covered = ready_runs_to_width_px(&far, 8000.0, 800);
        // On 8000 frames / 800 px each single frame is sub-pixel → both drop.
        assert!(exact.is_empty(), "sub-pixel Exact drops: {exact:?}");
        assert!(covered.is_empty(), "sub-pixel Covered drops: {covered:?}");
        // Visible multi-frame runs keep a real hole between them.
        let split = ready_runs_to_width_px(&[(0, 50), (200, 250)], 8000.0, 800);
        assert_eq!(split.len(), 2, "ready greens keep holes: {split:?}");
    }

    #[test]
    fn keyframe_ticks_inflate_to_one_px() {
        let far = [(0, 0), (100, 100)];
        let ticks = keyframe_runs_to_width_px(&far, 8000.0, 800);
        assert_eq!(ticks.len(), 2, "keyframe ticks stay visible: {ticks:?}");
        assert!(
            ticks.iter().all(|&(lo, hi)| hi == lo + 1),
            "each tick is 1 device px: {ticks:?}"
        );
    }

    #[test]
    fn sparse_ready_build_not_wider_than_coalesced() {
        // Long clip: sparse islands used to inflate to 1 px each and abut into a
        // longer band than the later coalesced run (capacity-looking trail).
        let max = 14_400.0;
        let w = 800u16;
        let sparse = [(100u64, 100), (102, 102), (104, 104), (106, 106)];
        let coalesced = [(100u64, 106)];
        let sparse_px = ready_runs_to_width_px(&sparse, max, w);
        let solid_px = ready_runs_to_width_px(&coalesced, max, w);
        let sparse_w: u32 = sparse_px
            .iter()
            .map(|&(lo, hi)| u32::from(hi.saturating_sub(lo)))
            .sum();
        let solid_w: u32 = solid_px
            .iter()
            .map(|&(lo, hi)| u32::from(hi.saturating_sub(lo)))
            .sum();
        assert!(
            sparse_w <= solid_w,
            "sparse build must not paint wider than coalesced: sparse={sparse_px:?} ({sparse_w}) solid={solid_px:?} ({solid_w})"
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
            stabilize_strip_segments_px(&prev, &[(0, 101)], STRIP_PAINT_STICKY_PX, w),
            prev
        );
        assert_eq!(
            stabilize_strip_segments_px(&prev, &[(1, 100)], STRIP_PAINT_STICKY_PX, w),
            prev
        );
        assert_eq!(
            stabilize_strip_segments_px(&prev, &[(0, 99)], STRIP_PAINT_STICKY_PX, w),
            prev
        );
    }

    #[test]
    fn stabilize_accepts_real_growth_past_sticky() {
        let prev = vec![(0, 100)];
        let next = vec![(0, 103)];
        assert_eq!(
            stabilize_strip_segments_px(&prev, &next, STRIP_PAINT_STICKY_PX, 800),
            next
        );
    }

    #[test]
    fn stabilize_accepts_clear_and_first_paint() {
        assert!(stabilize_strip_segments_px(&[(0, 10)], &[], 2, 800).is_empty());
        assert_eq!(
            stabilize_strip_segments_px(&[], &[(0, 10)], 2, 800),
            vec![(0, 10)]
        );
    }

    #[test]
    fn latch_kills_raf_edge_thrash() {
        let w = 800u16;
        let mut latch = StripPaintLatch::new();
        assert!(strip_runs_eq(
            latch.resolve(&[(0.0, 200.0)], w),
            &[(0.0, 200.0)]
        ));
        let mut unique = std::collections::BTreeSet::new();
        unique.insert(format!("{:?}", latch.accepted()));
        for i in 0..60 {
            let end = if i % 2 == 0 { 201.0 } else { 199.0 };
            let runs = latch.resolve(&[(0.0, end)], w).to_vec();
            unique.insert(format!("{runs:?}"));
        }
        assert_eq!(unique.len(), 1, "sub-sticky thrash must not rewrite runs");
        let grown = latch.resolve(&[(0.0, 220.0)], w).to_vec();
        assert!(strip_runs_eq(&grown, &[(0.0, 220.0)]));
        unique.insert(format!("{grown:?}"));
        assert_eq!(unique.len(), 2);
    }

    #[test]
    fn latch_fetch_fraction_jitter() {
        let w = 800u16;
        let wf = f64::from(w);
        let mut latch = StripPaintLatch::new();
        let a = fetch_strip_runs_for_width(&[(0.0, 0.5)], wf);
        let painted = latch.resolve(&a, w).to_vec();
        let mut unique = std::collections::BTreeSet::new();
        unique.insert(format!("{painted:?}"));
        for i in 0..30 {
            let alt = if i % 2 == 0 {
                fetch_strip_runs_for_width(&[(0.0, 0.50125)], wf)
            } else {
                fetch_strip_runs_for_width(&[(0.0, 0.5)], wf)
            };
            unique.insert(format!("{:?}", latch.resolve(&alt, w)));
        }
        assert_eq!(unique.len(), 1, "fetch f32 jitter: {unique:?}");
    }

    #[test]
    fn latch_holds_edge_thrash_on_visible_ready_runs() {
        let w = 800u16;
        let wf = f64::from(w);
        let a = ready_runs_to_width(&[(0, 399)], 8000.0, wf); // → 40.0
        let b = ready_runs_to_width(&[(0, 409)], 8000.0, wf); // → 41.0
        assert_eq!(a.len(), 1);
        assert!((a[0].1 - 40.0).abs() < 1e-9);
        assert!((b[0].1 - 41.0).abs() < 1e-9);
        let mut latch = StripPaintLatch::new();
        let _ = latch.resolve(&a, w);
        let mut unique = std::collections::BTreeSet::new();
        unique.insert(format!("{:?}", latch.accepted()));
        for i in 0..40 {
            let end = if i % 2 == 0 { 399u64 } else { 409 };
            let cand = ready_runs_to_width(&[(0, end)], 8000.0, wf);
            unique.insert(format!("{:?}", latch.resolve(&cand, w)));
        }
        assert_eq!(unique.len(), 1, "sub-sticky ready edge thrash must hold");
    }

    #[test]
    fn many_holes_float_runs_on_buffer() {
        // Long-clip FetchMap: continuous runs on real buffer (not CSS gradient).
        let w = 1280u16;
        let wf = f64::from(w);
        let mut fracs = Vec::new();
        for i in 0..200 {
            let a = i as f32 / 400.0;
            let b = a + 0.001;
            fracs.push((a, b.min(1.0)));
        }
        let runs = fetch_strip_runs_for_width(&fracs, wf);
        assert!(!runs.is_empty());
        for &(lo, hi) in &runs {
            assert!(lo < hi);
            assert!(hi <= wf + 1e-9);
        }
        assert!(runs.len() <= 200);
    }

    #[test]
    fn ready_keeps_equal_step_hairlines() {
        // Continuous grid: single-frame marks stay as w/n hairlines (not dropped).
        let far = [(0u64, 0), (100, 100)];
        let runs = ready_runs_to_width(&far, 8000.0, 800.0);
        assert_eq!(runs.len(), 2);
        assert!((runs[0].1 - runs[0].0 - 0.1).abs() < 1e-9);
        let split = ready_runs_to_width(&[(0, 50), (200, 250)], 8000.0, 800.0);
        assert_eq!(split.len(), 2, "ready greens keep holes: {split:?}");
    }

    #[test]
    fn frame_zebra_gate_is_step_at_least_min_slot() {
        assert_eq!(frame_zebra_slot_px(50, 100), 2);
        assert!(frame_slots_at_least_min(50, 100)); // step = 2
        assert_eq!(frame_zebra_slot_px(100, 100), 1);
        assert!(!frame_slots_at_least_min(100, 100)); // step = 1 < 1.5
        assert!(frame_slots_at_least_min(2, 3)); // step = 1.5
        assert!(!frame_slots_at_least_min(2, 2)); // step = 1
        assert_eq!(frame_zebra_slot_px(8000, 800), 0);
        assert!(!frame_slots_at_least_min(8000, 800));
        assert_eq!(frame_zebra_slot_px(0, 100), 0);
        assert!(frame_slots_at_least_min(1, 800));
        assert!(odd_frame_runs(100, 100.0).is_empty());
        assert!(!odd_frame_runs(50, 100.0).is_empty());
    }

    #[test]
    fn frame_slot_floor_matches_scrub_columns() {
        // w=100, n=3 — columns use floor(x/w·n).
        assert_eq!(frame_slot_floor_px(0, 3, 100), (0, 34));
        assert_eq!(frame_slot_floor_px(1, 3, 100), (34, 67));
        assert_eq!(frame_slot_floor_px(2, 3, 100), (67, 100));
        for x in 0..100u32 {
            let frame = (u64::from(x) * 3 / 100) as u32;
            let (lo, hi) = frame_slot_floor_px(frame, 3, 100);
            assert!(x >= lo && x < hi, "x={x} frame={frame} slot=[{lo},{hi})");
        }
        let (lo, hi) = frame_slot_floor_px(299, 300, 800);
        assert_eq!((lo, hi), (798, 800));
    }

    #[test]
    fn frame_slot_is_equal_step() {
        let (lo, hi) = frame_slot(1, 3, 100.0);
        assert!((lo - 100.0 / 3.0).abs() < 1e-9);
        assert!((hi - 200.0 / 3.0).abs() < 1e-9);
        assert!((frame_zebra_step(300, 800.0) - 800.0 / 300.0).abs() < 1e-9);
        let runs = odd_frame_runs(3, 100.0);
        assert_eq!(runs.len(), 1);
        assert!((runs[0].0 - 100.0 / 3.0).abs() < 1e-9);
        assert!((runs[0].1 - 200.0 / 3.0).abs() < 1e-9);
        assert!(odd_frame_runs(8000, 800.0).is_empty());
    }

    #[test]
    fn frame_slot_covers_last_frame() {
        // Last frame owns [(n-1)/n, 1) — not a point on the right edge.
        assert_eq!(frame_slot_px(9, 10, 100), (90, 100));
        assert_eq!(frame_slot_px(0, 10, 100), (0, 10));
    }
}
