//! Scrubber strip paint math: frame runs / fetch fractions → integer pixel runs.
//!
//! Hosts paint with a 1 px-tall canvas + integer `fillRect` (Chrome blurs
//! many-stop CSS gradients on long clips). mediabunny: no meter.

#[cfg(target_arch = "wasm32")]
use crate::video::{ReadyFrameKind, ReadyFrameMark};
#[cfg(target_arch = "wasm32")]
use wasm_bindgen::JsCast;
use webmedia::FrameId;

/// Dim Covered solid ≈ `#34d399` @ 0.35 over `#141414` (ready track).
pub const COVERED_STRIP_COLOR: &str = "#1f5742";

/// Bright Exact (full `#34d399`).
pub const EXACT_STRIP_COLOR: &str = "#34d399";

/// Gray FetchMap solid.
pub const FETCH_STRIP_COLOR: &str = "#6b7280";

/// Frame marks → snapped pixel runs on `width_px` (Covered soft-merges; Exact hard).
#[cfg(target_arch = "wasm32")]
pub fn cache_strip_px_runs_for_width(
    marks: &[ReadyFrameMark],
    kind: ReadyFrameKind,
    max: f64,
    width_px: u16,
) -> Vec<StripPxRun> {
    let mut runs: Vec<(FrameId, FrameId)> = marks
        .iter()
        .filter(|m| m.kind == kind)
        .map(|m| (m.frame, m.end.max(m.frame)))
        .collect();
    runs.sort_by_key(|&(lo, _)| lo);
    ready_runs_to_width(&runs, kind == ReadyFrameKind::Covered, max, width_px)
}

/// Sorted inclusive ready runs → snapped pixel runs on `width_px`.
/// `soft_merge` closes sub-pixel holes (Covered); Exact keeps every hole.
pub fn ready_runs_to_width(
    runs: &[(FrameId, FrameId)],
    soft_merge: bool,
    max: f64,
    width_px: u16,
) -> Vec<StripPxRun> {
    let max = max.max(1.0);
    let merge_gap = if soft_merge {
        strip_merge_gap_frames(max).saturating_mul(2)
    } else {
        0
    };
    frame_runs_to_width(runs, max, merge_gap, width_px)
}

/// Device-pixel buffer width for a 1 px-tall strip canvas (`css_w × dpr`).
pub fn strip_canvas_buffer_width(css_width: f64, device_pixel_ratio: f64) -> u16 {
    let dpr = device_pixel_ratio.max(1.0);
    let w = (css_width.max(0.0) * dpr).round();
    (w as u16).max(1)
}

/// Paint opaque runs into a **1 px-tall** canvas with integer `fillRect`.
/// Stretch the canvas in CSS to the strip height. `runs` must already be in
/// `buffer_width` space. Returns `false` if the 2d context is unavailable.
#[cfg(target_arch = "wasm32")]
pub fn paint_strip_canvas_1px(
    canvas: &web_sys::HtmlCanvasElement,
    buffer_width: u16,
    runs: &[StripPxRun],
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
        if hi <= lo {
            continue;
        }
        // Integer device pixels only — no subpixel fillRect.
        let x = f64::from(lo);
        let rw = f64::from(hi.saturating_sub(lo));
        ctx.fill_rect(x, 0.0, rw, 1.0);
    }
    true
}

/// Assumed scrubber CSS width for paint soft-merge (~1 px). Parity with
/// `webmedia` `source::fetch_map` gray-strip scaling.
const STRIP_ASSUMED_SCRUBBER_PX: f64 = 800.0;

/// Missing frames between inclusive runs that still paint as &lt;1 px on an
/// ~800 px scrubber (`floor(max / 800)`; short clips → 0 = no soft-merge).
pub fn strip_merge_gap_frames(max_frame: f64) -> u64 {
    let max = max_frame.max(0.0);
    (max / STRIP_ASSUMED_SCRUBBER_PX).floor() as u64
}

/// Paint-only: merge inclusive `(lo, hi)` densify runs when the hole is ≤
/// `max_gap` frames. Engine / densify geometry stays exact via
/// [`webmedia::coalesce_frame_runs`].
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_strips_are_empty_runs() {
        assert!(ready_runs_to_width(&[], false, 10.0, 800).is_empty());
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
    fn exact_does_not_soft_merge_holes() {
        let far = [(0, 0), (100, 100)];
        let exact = ready_runs_to_width(&far, false, 8000.0, 800);
        assert_eq!(exact.len(), 2, "Exact keeps holes: {exact:?}");
        let c = ready_runs_to_width(&far, true, 8000.0, 800);
        // soft-merge gap = 2*floor(8000/800)=20; hole 99 > 20 → two segments.
        assert_eq!(c.len(), 2, "Covered keeps real holes: {c:?}");
        let c2 = ready_runs_to_width(&[(0, 0), (15, 15)], true, 8000.0, 800);
        assert_eq!(c2.len(), 1, "Covered soft-merges ≤2px gap: {c2:?}");
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
}
