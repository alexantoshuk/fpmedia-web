//! Shared UI math for host player controls (not engine decode / not keyboard bindings).
//!
//! Scrubber strip pixel runs: [`crate::strip`] (re-exported here).

pub use crate::strip::*;
use fpmedia_web::VideoMetadata;

/// Playhead as a CSS width percentage of the scrubber (`0…100`).
pub fn playhead_width_pct(playhead: u32, frame_count: u32) -> f64 {
    let max = frame_count.saturating_sub(1).max(1) as f64;
    ((playhead as f64 / max) * 100.0).clamp(0.0, 100.0)
}

/// Same as [`playhead_width_pct`], formatted for `style:width`.
pub fn playhead_width_css(playhead: u32, frame_count: u32) -> String {
    format!("{:.3}%", playhead_width_pct(playhead, frame_count))
}

/// Min CSS width for the playhead tip. 1 px flickers on Chromium (device-pixel
/// round-trip); 2 px stays stable when the frame slot is sub-pixel.
pub const PLAYHEAD_TIP_MIN_PX: u32 = 2;

/// Integer CSS-pixel span for the opaque current-frame tip: `(left, width)`.
///
/// Same half-open snap as zebra / strip runs (`round(f/max·w)` …). Width is at
/// least [`PLAYHEAD_TIP_MIN_PX`]; on the last frame (point at the right edge)
/// the tip is pinned inside the track. Pass track width in **CSS** px (not
/// device px).
pub fn playhead_tip_px(playhead: u32, frame_count: u32, track_css_w: f64) -> (u32, u32) {
    let w = track_css_w.floor().max(0.0) as u32;
    if frame_count == 0 || w == 0 {
        return (0, 0);
    }
    let max = frame_count.saturating_sub(1);
    let min_w = PLAYHEAD_TIP_MIN_PX.min(w.max(1));
    if max == 0 {
        // Single frame: tip fills the track (still ≥ min).
        return (0, w.max(min_w));
    }
    let max_f = f64::from(max);
    let wf = f64::from(w);
    let f = playhead.min(max);
    if f >= max {
        return (w.saturating_sub(min_w), min_w);
    }
    let lo = ((f as f64 / max_f) * wf).round() as i32;
    let hi = (((f as f64 + 1.0) / max_f) * wf).round() as i32;
    let lo = lo.clamp(0, w as i32) as u32;
    let hi = hi.clamp(0, w as i32) as u32;
    let width = hi.saturating_sub(lo).max(min_w);
    let left = if lo.saturating_add(width) > w {
        w.saturating_sub(width)
    } else {
        lo
    };
    (left, width)
}

/// Same as [`playhead_tip_px`], formatted for `style:left` / `style:width`.
pub fn playhead_tip_css(playhead: u32, frame_count: u32, track_css_w: f64) -> (String, String) {
    let (left, width) = playhead_tip_px(playhead, frame_count, track_css_w);
    (format!("{left}px"), format!("{width}px"))
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

    #[test]
    fn playhead_pct_edges() {
        assert!((playhead_width_pct(0, 100) - 0.0).abs() < 1e-9);
        assert!((playhead_width_pct(99, 100) - 100.0).abs() < 1e-9);
    }

    #[test]
    fn playhead_tip_snaps_integer_px_and_floors_width() {
        // 101 frames → max=100; at 100 px each slot is 1 px → floor to 2 px.
        assert_eq!(playhead_tip_px(50, 101, 100.0), (50, 2));
        assert_eq!(playhead_tip_px(100, 101, 100.0), (98, 2));
        // Sub-pixel slots (8000 frames / 800 px): ≥2 px floor.
        let (lo, w) = playhead_tip_px(4000, 8000, 800.0);
        assert_eq!(w, 2);
        assert!(lo < 800);
        assert_eq!(playhead_tip_px(0, 0, 800.0), (0, 0));
        assert_eq!(
            playhead_tip_css(50, 101, 100.0),
            ("50px".into(), "2px".into())
        );
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
