//! Shared UI math for media chrome (not engine decode / not keyboard bindings).
//!
//! Scrubber strip pixel runs: [`crate::strip`] (re-exported here).

pub use crate::strip::*;
use webmedia::VideoMetadata;

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

    #[test]
    fn playhead_pct_edges() {
        assert!((playhead_width_pct(0, 100) - 0.0).abs() < 1e-9);
        assert!((playhead_width_pct(99, 100) - 100.0).abs() < 1e-9);
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
