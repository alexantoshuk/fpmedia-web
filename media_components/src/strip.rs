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

/// Frame marks → snapped pixel runs on `width_px` (honest: no soft-merge / no min-px inflate).
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
    ready_runs_to_width(&runs, max, width_px)
}

/// Sorted inclusive ready runs → snapped pixel runs on `width_px`.
/// View = data feedback: holes stay; sub-pixel runs drop (no inflate).
pub fn ready_runs_to_width(
    runs: &[(FrameId, FrameId)],
    max: f64,
    width_px: u16,
) -> Vec<StripPxRun> {
    frame_runs_to_width(runs, max.max(1.0), 0, width_px)
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
/// this many assumed scrubber pixels. Kills ±1 px densify / FetchMap swim
/// (“sometimes” blur). mediabunny: no equivalent meter.
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
///
/// `min_run_px == 0`: drop runs that round to empty (ready greens — honest).
/// `min_run_px >= 1`: inflate short runs (gray FetchMap — keep visible ticks).
pub fn snap_pct_runs_to_width(
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
    snap_pct_runs_to_width(segments, STRIP_ASSUMED_SCRUBBER_PX as u16, 1)
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
        1,
    )
}

/// File-byte fetch fractions → snapped runs on the assumed 800 px scrubber.
pub fn fetch_strip_px_runs(ranges: &[(f32, f32)]) -> Vec<StripPxRun> {
    fetch_strip_px_runs_for_width(ranges, STRIP_ASSUMED_SCRUBBER_PX as u16)
}

/// Inclusive frame runs → snapped scrubber pixels (after optional soft-merge).
/// Ready greens use `merge_gap = 0` + `min_run_px = 0` (view mirrors data).
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
        0,
    )
}

/// Inclusive frame runs → snapped pixels on the assumed 800 px scrubber.
pub fn frame_runs_to_px(runs: &[(FrameId, FrameId)], max: f64, merge_gap: u64) -> Vec<StripPxRun> {
    frame_runs_to_width(runs, max, merge_gap, STRIP_ASSUMED_SCRUBBER_PX as u16)
}

/// True when every frame slot `0 .. max` (`max = frame_count − 1`) rounds to ≥1
/// device pixel on `width_px`. Same half-open mapping as [`frame_runs_to_width`]
/// (the last frame id sits on the right edge). False for empty / single-frame.
pub fn frame_slots_at_least_1px(frame_count: u32, width_px: u16) -> bool {
    let max = frame_count.saturating_sub(1);
    if max == 0 || width_px == 0 {
        return false;
    }
    let w = f64::from(width_px);
    let max_f = f64::from(max);
    (0..max).all(|f| {
        let lo = ((f as f64 / max_f) * w).round() as i32;
        let hi = (((f as f64 + 1.0) / max_f) * w).round() as i32;
        hi > lo
    })
}

/// Odd presentation-frame slots as snapped pixel runs for a zebra track.
/// Empty when [`frame_slots_at_least_1px`] is false (sub-pixel frames).
///
/// Does **not** use [`snap_pct_runs_to_width`]'s abut/+1 coalesce — that would
/// bridge the even-frame gap and paint a solid bar.
pub fn odd_frame_px_runs(frame_count: u32, width_px: u16) -> Vec<StripPxRun> {
    if !frame_slots_at_least_1px(frame_count, width_px) {
        return Vec::new();
    }
    let width_px = width_px.max(1);
    let max = frame_count.saturating_sub(1);
    let w = f64::from(width_px);
    let max_f = f64::from(max);
    let mut out: Vec<StripPxRun> = Vec::new();
    for f in (1..max).step_by(2) {
        let mut lo = ((f as f64 / max_f) * w).round() as i32;
        let mut hi = (((f as f64 + 1.0) / max_f) * w).round() as i32;
        lo = lo.clamp(0, i32::from(width_px));
        hi = hi.clamp(0, i32::from(width_px));
        if hi <= lo {
            continue;
        }
        let lo = lo as u16;
        let hi = hi as u16;
        // Overlap / exact abut only — never bridge a 1 px even slot.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_strips_are_empty_runs() {
        assert!(ready_runs_to_width(&[], 10.0, 800).is_empty());
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
        let exact = ready_runs_to_width(&far, 8000.0, 800);
        let covered = ready_runs_to_width(&far, 8000.0, 800);
        // On 8000 frames / 800 px each single frame is sub-pixel → both drop.
        assert!(exact.is_empty(), "sub-pixel Exact drops: {exact:?}");
        assert!(covered.is_empty(), "sub-pixel Covered drops: {covered:?}");
        // Visible multi-frame runs keep a real hole between them.
        let split = ready_runs_to_width(&[(0, 50), (200, 250)], 8000.0, 800);
        assert_eq!(split.len(), 2, "ready greens keep holes: {split:?}");
    }

    #[test]
    fn sparse_ready_build_not_wider_than_coalesced() {
        // Long clip: sparse islands used to inflate to 1 px each and abut into a
        // longer band than the later coalesced run (capacity-looking trail).
        let max = 14_400.0;
        let w = 800u16;
        let sparse = [(100u64, 100), (102, 102), (104, 104), (106, 106)];
        let coalesced = [(100u64, 106)];
        let sparse_px = ready_runs_to_width(&sparse, max, w);
        let solid_px = ready_runs_to_width(&coalesced, max, w);
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
    fn latch_holds_edge_thrash_on_visible_ready_runs() {
        // Honest greens: multi-frame runs that survive sub-pixel drop; ±1 px edge.
        let w = 800u16;
        let a = ready_runs_to_width(&[(0, 399)], 8000.0, w); // → 40 px
        let b = ready_runs_to_width(&[(0, 409)], 8000.0, w); // → 41 px
        assert_eq!(a, vec![(0, 40)]);
        assert_eq!(b, vec![(0, 41)]);
        let mut latch = StripPaintLatch::new();
        let _ = latch.resolve(&a, w);
        let mut unique = std::collections::BTreeSet::new();
        unique.insert(format!("{:?}", latch.accepted()));
        for i in 0..40 {
            let end = if i % 2 == 0 { 399u64 } else { 409 };
            let cand = ready_runs_to_width(&[(0, end)], 8000.0, w);
            unique.insert(format!("{:?}", latch.resolve(&cand, w)));
        }
        assert_eq!(unique.len(), 1, "sub-sticky ready edge thrash must hold");
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
    fn frame_zebra_only_when_slots_are_full_pixels() {
        // 101 frames → max=100; at 100 px every slot is 1 px.
        assert!(frame_slots_at_least_1px(101, 100));
        let runs = odd_frame_px_runs(101, 100);
        assert_eq!(runs.len(), 50, "odd slots 1,3,…,99: {runs:?}");
        assert_eq!(runs[0], (1, 2));
        assert_eq!(runs[49], (99, 100));
        // Sub-pixel: long clip on narrow bar → off.
        assert!(!frame_slots_at_least_1px(8000, 800));
        assert!(odd_frame_px_runs(8000, 800).is_empty());
        assert!(!frame_slots_at_least_1px(1, 800));
        // Two frames → one slot (frame 0); last id on the edge → no odd column.
        assert!(frame_slots_at_least_1px(2, 1));
        assert!(odd_frame_px_runs(2, 1).is_empty());
    }
}
