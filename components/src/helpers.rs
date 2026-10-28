//! Shared UI math for host player controls (not engine decode / not keyboard bindings).
//!
//! Scrubber strip pixel runs: [`crate::strip`] (re-exported here).

pub use crate::strip::*;
use fpmedia_web::VideoMetadata;

/// Playhead as a CSS width percentage of the scrubber (`0…100`).
///
/// Left edge of the current-frame slot on the continuous `frame_count` grid.
pub fn playhead_width_pct(playhead: u32, frame_count: u32) -> f64 {
    let n = frame_count.max(1) as f64;
    let f = playhead.min(frame_count.saturating_sub(1)) as f64;
    ((f / n) * 100.0).clamp(0.0, 100.0)
}

/// Progress fill to tip left — continuous `calc(100% * f / n)` (no px snap).
pub fn playhead_width_css(playhead: u32, frame_count: u32) -> String {
    if frame_count == 0 {
        return "0".into();
    }
    let f = playhead.min(frame_count - 1);
    format!("calc(100% * {f} / {frame_count})")
}

/// CSS `left` / `width` for tip — continuous `f/n` left; width at least 2 px
/// (`max(2px, 100%/n)`). Host track clips overflow at EOF (`overflow: hidden`).
pub fn playhead_tip_css(playhead: u32, frame_count: u32) -> (String, String) {
    if frame_count == 0 {
        return ("0".into(), "0".into());
    }
    let n = frame_count;
    let f = playhead.min(n - 1);
    let left = format!("calc(100% * {f} / {n})");
    let width = format!("max(2px, calc(100% / {n}))");
    (left, width)
}

/// Map a pointer X on a scrubber element to a presentation frame.
///
/// Hits continuous equal slots `[f/n,(f+1)/n)` — same as [`frame_slot`].
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
    let n = frame_count;
    // x == w → floor(n) == n; clamp to last frame.
    let frame = ((x / w) * n as f64).floor() as u32;
    Some(frame.min(n - 1))
}

/// Min CSS px travel before a touch scrub move may update playhead (pad noise).
pub const TOUCH_SCRUB_DEADZONE_PX: f64 = 4.0;

/// Min `|Δframe|` for touch scrub ≈ one CSS pixel of the timeline (`ceil(n/w)`).
pub fn scrub_touch_min_frame_delta(frame_count: u32, track_width: f64) -> u32 {
    let w = track_width.max(1.0);
    let n = f64::from(frame_count.max(1));
    (n / w).ceil() as u32
}

/// Whether a touch scrub `pointermove` should accept mapped `frame` at track-relative `x`.
///
/// Requires both travel past [`TOUCH_SCRUB_DEADZONE_PX`] and
/// `|frame - last_frame| ≥ scrub_touch_min_frame_delta` so long clips do not
/// rewrite playhead on finger-pad jitter. Mouse/pen skip this gate.
pub fn scrub_touch_move_accepts(
    last_x: f64,
    x: f64,
    last_frame: u32,
    frame: u32,
    frame_count: u32,
    track_width: f64,
) -> bool {
    if (x - last_x).abs() < TOUCH_SCRUB_DEADZONE_PX {
        return false;
    }
    if frame == last_frame {
        return false;
    }
    let min_df = scrub_touch_min_frame_delta(frame_count, track_width);
    last_frame.abs_diff(frame) >= min_df
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
        // Left edge of last slot — not 100% (slot spans the final 1/n).
        assert!((playhead_width_pct(99, 100) - 99.0).abs() < 1e-9);
    }

    #[test]
    fn playhead_tip_uses_continuous_calc_slots() {
        assert_eq!(
            playhead_tip_css(1, 3),
            (
                "calc(100% * 1 / 3)".into(),
                "max(2px, calc(100% / 3))".into()
            )
        );
        assert_eq!(
            playhead_tip_css(299, 300),
            (
                "calc(100% * 299 / 300)".into(),
                "max(2px, calc(100% / 300))".into()
            )
        );
        assert_eq!(playhead_width_css(1, 3), "calc(100% * 1 / 3)");
        assert_eq!(playhead_tip_css(0, 0), ("0".into(), "0".into()));
    }

    #[test]
    fn scrub_x_maps_ends() {
        assert_eq!(scrub_frame_from_x(0.0, 0.0, 100.0, 11), Some(0));
        assert_eq!(scrub_frame_from_x(100.0, 0.0, 100.0, 11), Some(10));
        // Mid of frame-0 cell on 11-frame / 110 px track.
        assert_eq!(scrub_frame_from_x(5.0, 0.0, 110.0, 11), Some(0));
        assert_eq!(scrub_frame_from_x(15.0, 0.0, 110.0, 11), Some(1));
    }

    #[test]
    fn touch_min_frame_delta_scales_with_density() {
        // Short clip: wider than 1 px/frame → step 1.
        assert_eq!(scrub_touch_min_frame_delta(100, 400.0), 1);
        // Long clip on phone-wide track: ≈ n/w.
        assert_eq!(scrub_touch_min_frame_delta(43_200, 360.0), 120);
    }

    #[test]
    fn touch_move_rejects_pad_jitter() {
        let n = 43_200;
        let w = 360.0;
        // ±2 px contact noise — under deadzone.
        assert!(!scrub_touch_move_accepts(
            100.0, 102.0, 12_000, 12_240, n, w
        ));
        // 4 px travel but same frame — no-op.
        assert!(!scrub_touch_move_accepts(
            100.0, 104.0, 12_000, 12_000, n, w
        ));
        // 4 px + ≥1 px of frames → accept.
        assert!(scrub_touch_move_accepts(100.0, 104.0, 12_000, 12_480, n, w));
    }

    #[test]
    fn hover_thumb_clamps_to_track() {
        assert!((hover_thumb_left(0.0, 400.0, 160.0) - 0.0).abs() < 1e-9);
        assert!((hover_thumb_left(200.0, 400.0, 160.0) - 120.0).abs() < 1e-9);
        assert!((hover_thumb_left(400.0, 400.0, 160.0) - 240.0).abs() < 1e-9);
        assert_eq!(hover_thumb_transform_css(120.0), "translate3d(120px, 0, 0)");
    }
}
