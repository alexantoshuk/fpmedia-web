//! Shared UI math for media chrome (not engine decode / not keyboard bindings).
//!
//! Converts engine marks / fetch fractions into scrubber strip pixel runs.
//! Hosts paint with a 1 px-tall canvas + integer `fillRect` (Chrome blurs
//! many-stop CSS gradients on long clips). mediabunny: no meter.

use crate::video::{ReadyFrameKind, ReadyFrameMark, VideoMetadata};
use wasm_bindgen::JsCast;
use webmedia::frame_runs_to_width;
use webmedia::strip_merge_gap_frames;

pub use webmedia::{
    STRIP_PAINT_STICKY_PX, StripPaintLatch, StripPxRun, fetch_strip_px_runs,
    fetch_strip_px_runs_for_width, frame_runs_to_px, snap_pct_runs_to_px, snap_pct_runs_to_width,
    stabilize_strip_segments, strip_raster_diff_px,
};

/// Dim Covered solid ≈ `#34d399` @ 0.35 over `#141414` (ready track).
pub const COVERED_STRIP_COLOR: &str = "#1f5742";

/// Bright Exact (full `#34d399`).
pub const EXACT_STRIP_COLOR: &str = "#34d399";

/// Gray FetchMap solid.
pub const FETCH_STRIP_COLOR: &str = "#6b7280";

/// Frame marks → snapped pixel runs on `width_px` (Covered soft-merges; Exact hard).
pub fn cache_strip_px_runs_for_width(
    marks: &[ReadyFrameMark],
    kind: ReadyFrameKind,
    max: f64,
    width_px: u16,
) -> Vec<StripPxRun> {
    let max = max.max(1.0);
    let merge_gap = match kind {
        ReadyFrameKind::Covered => strip_merge_gap_frames(max).saturating_mul(2),
        ReadyFrameKind::Exact => 0,
    };
    let mut runs: Vec<(u64, u64)> = marks
        .iter()
        .filter(|m| m.kind == kind)
        .map(|m| (m.frame, m.end.max(m.frame)))
        .collect();
    runs.sort_by_key(|&(lo, _)| lo);
    frame_runs_to_width(&runs, max, merge_gap, width_px)
}

/// Assumed-800 snap (tests / soft-merge parity).
pub fn cache_strip_px_runs(
    marks: &[ReadyFrameMark],
    kind: ReadyFrameKind,
    max: f64,
) -> Vec<StripPxRun> {
    cache_strip_px_runs_for_width(marks, kind, max, 800)
}

/// Device-pixel buffer width for a 1 px-tall strip canvas (`css_w × dpr`).
pub fn strip_canvas_buffer_width(css_width: f64, device_pixel_ratio: f64) -> u16 {
    let dpr = device_pixel_ratio.max(1.0);
    let w = (css_width.max(0.0) * dpr).round();
    (w as u16).max(1)
}

/// Paint opaque runs into a **1 px-tall** canvas with integer `fillRect`.
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

/// Playhead as a CSS width percentage of the scrubber (`0…100`).
pub fn playhead_width_pct(playhead: u32, frame_count: u32) -> f64 {
    let max = frame_count.saturating_sub(1).max(1) as f64;
    ((playhead as f64 / max) * 100.0).clamp(0.0, 100.0)
}

/// Same as [`playhead_width_pct`], formatted for `style:width`.
pub fn playhead_width_css(playhead: u32, frame_count: u32) -> String {
    format!("{:.3}%", playhead_width_pct(playhead, frame_count))
}

/// Map a pointer X on a scrubber element to a presentation frame.
pub fn scrub_frame_from_x(
    client_x: f64,
    rect_left: f64,
    rect_width: f64,
    frame_count: u32,
) -> Option<u32> {
    if frame_count == 0 {
        return None;
    }
    let w = rect_width.max(1.0);
    let x = (client_x - rect_left).clamp(0.0, w);
    let max_frame = frame_count.saturating_sub(1);
    let frame = ((x / w) * max_frame as f64).round() as u32;
    Some(frame.min(max_frame))
}

/// Clamp hover-thumb left so the preview stays inside the scrubber track.
pub fn hover_thumb_left(x: f64, track_width: f64, thumb_w: f64) -> f64 {
    let thumb_w = thumb_w.max(0.0);
    (x - thumb_w / 2.0).clamp(0.0, (track_width - thumb_w).max(0.0))
}

/// CSS `transform` for compositor-side hover-thumb tracking (not `left`).
pub fn hover_thumb_transform_css(left: f64) -> String {
    format!("translate3d({left}px, 0, 0)")
}

/// Compact source line for demo / host status (`320×240 · 24 fps · …`).
pub fn format_metadata(meta: &VideoMetadata) -> String {
    let fps = meta
        .fps
        .map(|v| format!("{v:.3} fps"))
        .unwrap_or_else(|| "fps —".into());
    let codec = meta
        .codec
        .codec_string
        .clone()
        .unwrap_or_else(|| format!("{:?}", meta.codec.codec));
    format!(
        "{}×{} · {} · {} · {:.3}s · {} frames",
        meta.width,
        meta.height,
        fps,
        codec,
        meta.duration().seconds,
        meta.frame_count
    )
}

/// SMPTE string for `frame` using embedded start (or zero) + rate from metadata.
pub fn format_timecode_at(meta: &VideoMetadata, frame: u32) -> Option<String> {
    meta.timecode_at(u64::from(frame)).map(|tc| tc.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::video::ReadyFrameKind;

    #[test]
    fn playhead_pct_edges() {
        assert!((playhead_width_pct(0, 100) - 0.0).abs() < 1e-9);
        assert!((playhead_width_pct(99, 100) - 100.0).abs() < 1e-9);
    }

    #[test]
    fn empty_strips_are_empty_runs() {
        assert!(cache_strip_px_runs(&[], ReadyFrameKind::Exact, 10.0).is_empty());
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
        use crate::video::ReadyFrameMark;
        let marks = [
            ReadyFrameMark::run(0, 0, ReadyFrameKind::Exact),
            ReadyFrameMark::run(100, 100, ReadyFrameKind::Exact),
        ];
        let exact = cache_strip_px_runs(&marks, ReadyFrameKind::Exact, 8000.0);
        assert_eq!(exact.len(), 2, "Exact keeps holes: {exact:?}");
        let covered = [
            ReadyFrameMark::run(0, 0, ReadyFrameKind::Covered),
            ReadyFrameMark::run(100, 100, ReadyFrameKind::Covered),
        ];
        let c = cache_strip_px_runs(&covered, ReadyFrameKind::Covered, 8000.0);
        // soft-merge gap = 2*floor(8000/800)=20; hole 99 > 20 → two segments.
        assert_eq!(c.len(), 2, "Covered keeps real holes: {c:?}");
        let covered_close = [
            ReadyFrameMark::run(0, 0, ReadyFrameKind::Covered),
            ReadyFrameMark::run(15, 15, ReadyFrameKind::Covered),
        ];
        let c2 = cache_strip_px_runs(&covered_close, ReadyFrameKind::Covered, 8000.0);
        assert_eq!(c2.len(), 1, "Covered soft-merges ≤2px gap: {c2:?}");
    }

    #[test]
    fn scrub_x_maps_ends() {
        assert_eq!(scrub_frame_from_x(0.0, 0.0, 100.0, 11), Some(0));
        assert_eq!(scrub_frame_from_x(100.0, 0.0, 100.0, 11), Some(10));
    }

    #[test]
    fn hover_thumb_clamps_to_track() {
        assert!((hover_thumb_left(0.0, 400.0, 160.0) - 0.0).abs() < 1e-9);
        assert!((hover_thumb_left(200.0, 400.0, 160.0) - 120.0).abs() < 1e-9);
        assert!((hover_thumb_left(400.0, 400.0, 160.0) - 240.0).abs() < 1e-9);
        assert_eq!(hover_thumb_transform_css(120.0), "translate3d(120px, 0, 0)");
    }
}
